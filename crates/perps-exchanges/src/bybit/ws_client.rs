use super::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use std::str::FromStr;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use url::Url;

const WS_BASE_URL: &str = "wss://stream.bybit.com/v5/public/linear";

/// Bybit WebSocket streaming client
#[derive(Clone)]
pub struct BybitWsClient {
    base_url: String,
}

impl BybitWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
        }
    }

    /// Connect to WebSocket and return the stream
    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Bybit WebSocket: {}", self.base_url);
        let url = Url::parse(&self.base_url)?;
        let (ws_stream, response) = connect_async(url.as_str()).await?;
        tracing::info!("Connected to Bybit WebSocket (status: {:?})", response.status());
        Ok(ws_stream)
    }

    /// Subscribe to topics
    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        topics: Vec<String>,
    ) -> Result<()> {
        let request = BybitWsSubscribeRequest {
            op: "subscribe".to_string(),
            args: topics,
        };
        let sub_message = serde_json::to_string(&request)?;
        tracing::debug!("Subscribing: {}", sub_message);
        ws_stream.send(Message::Text(sub_message)).await?;
        Ok(())
    }

    /// Convert Bybit ticker to our Ticker type
    fn convert_ticker(&self, ws_ticker: &BybitWsTicker) -> Result<Ticker> {
        let last_price = Decimal::from_str(&ws_ticker.last_price)?;
        let prev_price = if !ws_ticker.prev_price24h.is_empty() {
            Decimal::from_str(&ws_ticker.prev_price24h)?
        } else {
            last_price
        };
        let price_change = last_price - prev_price;

        Ok(Ticker {
            symbol: ws_ticker.symbol.clone(),
            last_price,
            mark_price: Decimal::from_str(&ws_ticker.mark_price)?,
            index_price: Decimal::from_str(&ws_ticker.index_price)?,
            best_bid_price: Decimal::from_str(&ws_ticker.bid1_price)?,
            best_bid_qty: Decimal::from_str(&ws_ticker.bid1_size)?,
            best_ask_price: Decimal::from_str(&ws_ticker.ask1_price)?,
            best_ask_qty: Decimal::from_str(&ws_ticker.ask1_size)?,
            volume_24h: Decimal::from_str(&ws_ticker.volume24h)?,
            turnover_24h: Decimal::from_str(&ws_ticker.turnover24h)?,
            open_interest: Decimal::ZERO,
            open_interest_notional: Decimal::ZERO,
            price_change_24h: price_change,
            price_change_pct: Decimal::from_str(&ws_ticker.price24h_pcnt)?,
            high_price_24h: Decimal::from_str(&ws_ticker.high_price24h)?,
            low_price_24h: Decimal::from_str(&ws_ticker.low_price24h)?,
            timestamp: Utc::now(),
        })
    }

    /// Convert Bybit trade to our Trade type
    fn convert_trade(&self, ws_trade: &BybitWsTrade) -> Result<Trade> {
        Ok(Trade {
            id: ws_trade.id.clone(),
            symbol: ws_trade.symbol.clone(),
            price: Decimal::from_str(&ws_trade.price)?,
            quantity: Decimal::from_str(&ws_trade.size)?,
            side: match ws_trade.side.as_str() {
                "Buy" => OrderSide::Buy,
                "Sell" => OrderSide::Sell,
                _ => OrderSide::Buy,
            },
            timestamp: Utc.timestamp_millis_opt(ws_trade.timestamp).unwrap(),
        })
    }

    /// Convert Bybit orderbook to our Orderbook type
    fn convert_orderbook(&self, ws_book: &BybitWsOrderbook) -> Result<Orderbook> {
        let bids: Vec<OrderbookLevel> = ws_book.b
            .iter()
            .map(|level| {
                if level.len() >= 2 {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level[0])?,
                        quantity: Decimal::from_str(&level[1])?,
                    })
                } else {
                    Err(anyhow!("Invalid bid level format"))
                }
            })
            .collect::<Result<Vec<_>>>()?;

        let asks: Vec<OrderbookLevel> = ws_book.a
            .iter()
            .map(|level| {
                if level.len() >= 2 {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level[0])?,
                        quantity: Decimal::from_str(&level[1])?,
                    })
                } else {
                    Err(anyhow!("Invalid ask level format"))
                }
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: ws_book.s.clone(),
            bids,
            asks,
            timestamp: Utc.timestamp_millis_opt(ws_book.timestamp).unwrap(),
        })
    }
}

impl Default for BybitWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for BybitWsClient {
    fn get_name(&self) -> &str {
        "bybit"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to tickers for each symbol
        let topics: Vec<String> = symbols.iter()
            .map(|s| format!("tickers.{}", s))
            .collect();
        self.subscribe(&mut ws_stream, topics).await?;

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        tracing::debug!("Received message: {}", &text[..text.len().min(200)]);

                        // Try to parse as subscription response first
                        if let Ok(sub_response) = serde_json::from_str::<BybitWsSubscriptionResponse>(&text) {
                            if sub_response.success {
                                tracing::debug!("Subscription confirmed: {}", sub_response.ret_msg);
                            } else {
                                tracing::error!("Subscription failed: {}", sub_response.ret_msg);
                            }
                            continue;
                        }

                        // Parse as data response
                        if let Ok(response) = serde_json::from_str::<BybitWsResponse>(&text) {
                            if response.topic.starts_with("tickers.") {
                                if let Ok(ticker) = serde_json::from_value::<BybitWsTicker>(response.data) {
                                    match client.convert_ticker(&ticker) {
                                        Ok(ticker) => yield Ok(ticker),
                                        Err(e) => yield Err(anyhow!("Failed to convert ticker: {}", e)),
                                    }
                                }
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

        // Subscribe to trades for each symbol
        let topics: Vec<String> = symbols.iter()
            .map(|s| format!("publicTrade.{}", s))
            .collect();
        self.subscribe(&mut ws_stream, topics).await?;

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Skip subscription responses
                        if text.contains("\"op\":\"subscribe\"") {
                            continue;
                        }

                        if let Ok(response) = serde_json::from_str::<BybitWsResponse>(&text) {
                            if response.topic.starts_with("publicTrade.") {
                                // Data is an array of trades
                                if let Ok(trades) = serde_json::from_value::<Vec<BybitWsTrade>>(response.data) {
                                    for ws_trade in trades {
                                        match client.convert_trade(&ws_trade) {
                                            Ok(trade) => yield Ok(trade),
                                            Err(e) => yield Err(anyhow!("Failed to convert trade: {}", e)),
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

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to orderbook for each symbol (depth 50)
        let topics: Vec<String> = symbols.iter()
            .map(|s| format!("orderbook.50.{}", s))
            .collect();
        self.subscribe(&mut ws_stream, topics).await?;

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Skip subscription responses
                        if text.contains("\"op\":\"subscribe\"") {
                            continue;
                        }

                        if let Ok(response) = serde_json::from_str::<BybitWsResponse>(&text) {
                            if response.topic.starts_with("orderbook.") {
                                if let Ok(ws_book) = serde_json::from_value::<BybitWsOrderbook>(response.data) {
                                    match client.convert_orderbook(&ws_book) {
                                        Ok(orderbook) => yield Ok(orderbook),
                                        Err(e) => yield Err(anyhow!("Failed to convert orderbook: {}", e)),
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

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut ws_stream = self.connect().await?;

        // Build topics list
        let mut topics = Vec::new();
        for symbol in &config.symbols {
            for data_type in &config.data_types {
                let topic = match data_type {
                    StreamDataType::Ticker => format!("tickers.{}", symbol),
                    StreamDataType::Trade => format!("publicTrade.{}", symbol),
                    StreamDataType::Orderbook => format!("orderbook.50.{}", symbol),
                    StreamDataType::FundingRate => continue, // Handled separately, requires different topic
                    StreamDataType::Kline => continue, // Klines not yet implemented for Bybit WebSocket
                };
                topics.push(topic);
            }
        }

        self.subscribe(&mut ws_stream, topics).await?;

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Skip subscription responses
                        if text.contains("\"op\":\"subscribe\"") {
                            continue;
                        }

                        if let Ok(response) = serde_json::from_str::<BybitWsResponse>(&text) {
                            if response.topic.starts_with("tickers.") {
                                if let Ok(ticker) = serde_json::from_value::<BybitWsTicker>(response.data) {
                                    if let Ok(ticker) = client.convert_ticker(&ticker) {
                                        yield Ok(StreamEvent::Ticker(ticker));
                                    }
                                }
                            } else if response.topic.starts_with("publicTrade.") {
                                if let Ok(trades) = serde_json::from_value::<Vec<BybitWsTrade>>(response.data) {
                                    for ws_trade in trades {
                                        if let Ok(trade) = client.convert_trade(&ws_trade) {
                                            yield Ok(StreamEvent::Trade(trade));
                                        }
                                    }
                                }
                            } else if response.topic.starts_with("orderbook.") {
                                if let Ok(ws_book) = serde_json::from_value::<BybitWsOrderbook>(response.data) {
                                    if let Ok(orderbook) = client.convert_orderbook(&ws_book) {
                                        yield Ok(StreamEvent::Orderbook(orderbook));
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
