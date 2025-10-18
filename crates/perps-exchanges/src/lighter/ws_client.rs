use super::ws_types::*;
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

const WS_BASE_URL: &str = "wss://mainnet.zklighter.elliot.ai/stream";

/// Lighter WebSocket streaming client
#[derive(Clone)]
pub struct LighterWsClient {
    base_url: String,
    /// Cache for symbol to market_id mapping
    symbol_to_market_id: HashMap<String, u64>,
}

impl LighterWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
            symbol_to_market_id: HashMap::new(),
        }
    }

    /// Connect to WebSocket and return the stream
    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Lighter WebSocket: {}", self.base_url);
        let (ws_stream, response) = connect_async(&self.base_url).await?;
        tracing::info!(
            "Connected to Lighter WebSocket (status: {:?})",
            response.status()
        );
        Ok(ws_stream)
    }

    /// Subscribe to a channel
    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        channel: String,
    ) -> Result<()> {
        let request = LighterWsSubscribeRequest {
            msg_type: "subscribe".to_string(),
            channel,
        };
        let sub_message = serde_json::to_string(&request)?;
        tracing::debug!("Subscribing: {}", sub_message);
        ws_stream.send(Message::Text(sub_message)).await?;
        Ok(())
    }

    /// Get market_id for a symbol by fetching from REST API
    async fn get_market_id(&mut self, symbol: &str) -> Result<u64> {
        // Check cache first
        if let Some(&market_id) = self.symbol_to_market_id.get(symbol) {
            return Ok(market_id);
        }

        // Fetch from REST API
        let url = "https://mainnet.zklighter.elliot.ai/api/v1/orderBooks";
        let response = reqwest::get(url).await?;
        let data: serde_json::Value = response.json().await?;

        // Parse the response (note: response structure is different - no "data" wrapper)
        if let Some(order_books) = data["order_books"].as_array() {
            for ob in order_books {
                if let (Some(sym), Some(id)) = (ob["symbol"].as_str(), ob["market_id"].as_u64()) {
                    self.symbol_to_market_id.insert(sym.to_string(), id);
                }
            }
        }

        // Try again from cache
        self.symbol_to_market_id
            .get(symbol)
            .copied()
            .ok_or_else(|| anyhow!("Symbol {} not found", symbol))
    }

    /// Convert Lighter market stats to our Ticker type
    fn convert_ticker(&self, stats: &LighterMarketStatsData) -> Result<Ticker> {
        let last_price = Decimal::from_f64(stats.last_trade_price)
            .ok_or_else(|| anyhow!("Invalid last_trade_price"))?;
        let mark_price =
            Decimal::from_f64(stats.mark_price).ok_or_else(|| anyhow!("Invalid mark_price"))?;
        let index_price =
            Decimal::from_f64(stats.index_price).ok_or_else(|| anyhow!("Invalid index_price"))?;

        // Calculate price change from percentage
        let price_change_pct = Decimal::from_f64(stats.daily_price_change).unwrap_or(Decimal::ZERO)
            / Decimal::from(100);
        let price_change_24h = price_change_pct * last_price;

        Ok(Ticker {
            symbol: stats.symbol.clone(),
            last_price,
            mark_price,
            index_price,
            best_bid_price: Decimal::ZERO, // Not available in market stats
            best_bid_qty: Decimal::ZERO,
            best_ask_price: Decimal::ZERO,
            best_ask_qty: Decimal::ZERO,
            volume_24h: Decimal::from_f64(stats.daily_volume).unwrap_or(Decimal::ZERO),
            turnover_24h: Decimal::from_f64(stats.daily_quote_volume).unwrap_or(Decimal::ZERO),
            open_interest: Decimal::ZERO,
            open_interest_notional: Decimal::ZERO,
            price_change_24h,
            price_change_pct,
            high_price_24h: Decimal::from_f64(stats.daily_price_high).unwrap_or(Decimal::ZERO),
            low_price_24h: Decimal::from_f64(stats.daily_price_low).unwrap_or(Decimal::ZERO),
            timestamp: if let Some(ts) = stats.timestamp {
                Utc.timestamp_millis_opt(ts).unwrap()
            } else {
                Utc::now()
            },
        })
    }

    /// Convert Lighter trade to our Trade type
    fn convert_trade(&self, trade: &LighterTradeData) -> Result<Trade> {
        Ok(Trade {
            id: trade.trade_id.clone(),
            symbol: trade.symbol.clone(),
            price: Decimal::from_str(&trade.price)?,
            quantity: Decimal::from_str(&trade.size)?,
            side: match trade.side.as_str() {
                "buy" => OrderSide::Buy,
                "sell" => OrderSide::Sell,
                _ => OrderSide::Buy,
            },
            timestamp: Utc.timestamp_millis_opt(trade.timestamp).unwrap(),
        })
    }

    /// Convert Lighter orderbook to our Orderbook type
    fn convert_orderbook(&self, symbol: &str, ob: &LighterOrderBook) -> Result<Orderbook> {
        let bids: Vec<OrderbookLevel> = ob
            .bids
            .iter()
            .map(|level| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&level.price)?,
                    quantity: Decimal::from_str(&level.size)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks: Vec<OrderbookLevel> = ob
            .asks
            .iter()
            .map(|level| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(&level.price)?,
                    quantity: Decimal::from_str(&level.size)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: symbol.to_string(),
            bids,
            asks,
            timestamp: Utc::now(),
        })
    }
}

impl Default for LighterWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for LighterWsClient {
    fn get_name(&self) -> &str {
        "lighter"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        // Get market IDs for all symbols
        let mut market_ids = Vec::new();
        for symbol in &symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_ids.push(market_id);
        }

        // Subscribe to market stats for each market
        for market_id in &market_ids {
            let channel = format!("market_stats/{}", market_id);
            self.subscribe(&mut ws_stream, channel).await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await; // Skip first tick

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                tracing::debug!("Received message: {}", &text[..text.len().min(200)]);

                                if let Ok(stats_msg) = serde_json::from_str::<LighterWsMarketStats>(&text) {
                                    match client.convert_ticker(&stats_msg.market_stats) {
                                        Ok(ticker) => yield Ok(ticker),
                                        Err(e) => yield Err(anyhow!("Failed to convert ticker: {}", e)),
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    tracing::error!("Failed to send pong: {}", e);
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                // Received pong response to our ping
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended (connection closed by server)");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        // Send periodic ping to keep connection alive
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        // Get market IDs for all symbols
        let mut market_ids = Vec::new();
        for symbol in &symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_ids.push(market_id);
        }

        // Subscribe to trades for each market
        for market_id in &market_ids {
            let channel = format!("trade/{}", market_id);
            self.subscribe(&mut ws_stream, channel).await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await; // Skip first tick

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(trade_msg) = serde_json::from_str::<LighterWsTrade>(&text) {
                                    for trade in &trade_msg.trades {
                                        match client.convert_trade(trade) {
                                            Ok(trade) => yield Ok(trade),
                                            Err(e) => yield Err(anyhow!("Failed to convert trade: {}", e)),
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended (connection closed by server)");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        // Get market IDs and create symbol lookup
        let mut market_id_to_symbol: HashMap<u64, String> = HashMap::new();
        for symbol in &symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_id_to_symbol.insert(market_id, symbol.clone());

            let channel = format!("order_book/{}", market_id);
            self.subscribe(&mut ws_stream, channel).await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await; // Skip first tick

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(ob_msg) = serde_json::from_str::<LighterWsOrderbook>(&text) {
                                    // Extract market_id from channel (e.g., "order_book:0" -> 0)
                                    if let Some(market_id_str) = ob_msg.channel.strip_prefix("order_book:") {
                                        if let Ok(market_id) = market_id_str.parse::<u64>() {
                                            if let Some(symbol) = market_id_to_symbol.get(&market_id) {
                                                match client.convert_orderbook(symbol, &ob_msg.order_book) {
                                                    Ok(orderbook) => yield Ok(orderbook),
                                                    Err(e) => yield Err(anyhow!("Failed to convert orderbook: {}", e)),
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended (connection closed by server)");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut ws_stream = self.connect().await?;
        let mut client = self.clone();

        // Get market IDs and create lookups
        let mut market_id_to_symbol: HashMap<u64, String> = HashMap::new();
        for symbol in &config.symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_id_to_symbol.insert(market_id, symbol.clone());

            // Subscribe to requested topics for this market
            for data_type in &config.data_types {
                let channel = match data_type {
                    StreamDataType::Ticker => format!("market_stats/{}", market_id),
                    StreamDataType::Trade => format!("trade/{}", market_id),
                    StreamDataType::Orderbook => format!("order_book/{}", market_id),
                    StreamDataType::FundingRate => continue, // Not available via WebSocket
                    StreamDataType::Kline => continue, // Klines not yet implemented for Lighter WebSocket
                };
                self.subscribe(&mut ws_stream, channel).await?;
            }
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await; // Skip first tick

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                // Try to parse as different message types
                                if let Ok(stats_msg) = serde_json::from_str::<LighterWsMarketStats>(&text) {
                                    if let Ok(ticker) = client.convert_ticker(&stats_msg.market_stats) {
                                        yield Ok(StreamEvent::Ticker(ticker));
                                    }
                                } else if let Ok(trade_msg) = serde_json::from_str::<LighterWsTrade>(&text) {
                                    for trade in &trade_msg.trades {
                                        if let Ok(trade) = client.convert_trade(trade) {
                                            yield Ok(StreamEvent::Trade(trade));
                                        }
                                    }
                                } else if let Ok(ob_msg) = serde_json::from_str::<LighterWsOrderbook>(&text) {
                                    // Extract market_id from channel
                                    if let Some(market_id_str) = ob_msg.channel.strip_prefix("order_book:") {
                                        if let Ok(market_id) = market_id_str.parse::<u64>() {
                                            if let Some(symbol) = market_id_to_symbol.get(&market_id) {
                                                if let Ok(orderbook) = client.convert_orderbook(symbol, &ob_msg.order_book) {
                                                    yield Ok(StreamEvent::Orderbook(orderbook));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                tracing::trace!("Received pong from Lighter");
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("WebSocket connection closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::warn!("Lighter WebSocket stream ended (connection closed by server)");
                                yield Err(anyhow!("WebSocket stream ended"));
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = keepalive.tick() => {
                        if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                            tracing::error!("Failed to send keepalive ping: {}", e);
                            yield Err(anyhow!("Failed to send keepalive ping: {}", e));
                            break;
                        }
                        tracing::trace!("Sent keepalive ping to Lighter");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
