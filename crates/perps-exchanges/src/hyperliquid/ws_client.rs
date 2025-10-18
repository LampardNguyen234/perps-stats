use super::ws_types::{
    HyperliquidWsBook, HyperliquidWsResponse, HyperliquidWsSubscribeRequest,
    HyperliquidWsSubscription, HyperliquidWsTrade,
};
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

const WS_BASE_URL: &str = "wss://api.hyperliquid.xyz/ws";

/// Hyperliquid WebSocket streaming client
#[derive(Clone)]
pub struct HyperliquidWsClient {
    base_url: String,
}

impl HyperliquidWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
        }
    }

    /// Connect to WebSocket and return the stream
    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Hyperliquid WebSocket: {}", self.base_url);
        let url = Url::parse(&self.base_url)?;
        let (ws_stream, response) = connect_async(url.as_str()).await?;
        tracing::info!(
            "Connected to Hyperliquid WebSocket (status: {:?})",
            response.status()
        );
        Ok(ws_stream)
    }

    /// Subscribe to a channel
    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        subscription: HyperliquidWsSubscription,
    ) -> Result<()> {
        let request = HyperliquidWsSubscribeRequest {
            method: "subscribe".to_string(),
            subscription,
        };
        let sub_message = serde_json::to_string(&request)?;
        tracing::debug!("Subscribing: {}", sub_message);
        ws_stream.send(Message::Text(sub_message)).await?;
        Ok(())
    }

    /// Convert Hyperliquid trade to our Trade type
    fn convert_trade(&self, ws_trade: &HyperliquidWsTrade) -> Result<Trade> {
        Ok(Trade {
            id: ws_trade.hash.clone(),
            symbol: ws_trade.coin.clone(),
            price: Decimal::from_str(&ws_trade.px)?,
            quantity: Decimal::from_str(&ws_trade.sz)?,
            side: match ws_trade.side.as_str() {
                "A" => OrderSide::Sell, // Ask = Sell
                "B" => OrderSide::Buy,  // Bid = Buy
                _ => OrderSide::Buy,
            },
            timestamp: Utc.timestamp_millis_opt(ws_trade.time).unwrap(),
        })
    }

    /// Convert Hyperliquid L2 book to our Orderbook type
    fn convert_orderbook(&self, ws_book: &HyperliquidWsBook) -> Result<Orderbook> {
        // levels[0] = bids, levels[1] = asks
        let bids: Vec<OrderbookLevel> = if ws_book.levels.len() > 0 {
            ws_book.levels[0]
                .iter()
                .map(|level| {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level.px)?,
                        quantity: Decimal::from_str(&level.sz)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        let asks: Vec<OrderbookLevel> = if ws_book.levels.len() > 1 {
            ws_book.levels[1]
                .iter()
                .map(|level| {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level.px)?,
                        quantity: Decimal::from_str(&level.sz)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        Ok(Orderbook {
            symbol: ws_book.coin.clone(),
            bids,
            asks,
            timestamp: Utc.timestamp_millis_opt(ws_book.time).unwrap(),
        })
    }
}

impl Default for HyperliquidWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for HyperliquidWsClient {
    fn get_name(&self) -> &str {
        "hyperliquid"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to allMids for ticker-like data
        self.subscribe(&mut ws_stream, HyperliquidWsSubscription::AllMids)
            .await?;

        let _client = self.clone();
        let symbols_set: std::collections::HashSet<String> = symbols.into_iter().collect();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        tracing::debug!("Received message: {}", &text[..text.len().min(200)]);

                        // Try to parse as allMids response
                        if let Ok(response) = serde_json::from_str::<HyperliquidWsResponse>(&text) {
                            match response {
                                HyperliquidWsResponse::AllMids(all_mids) => {
                                    // Parse mids object
                                    if let Some(mids_obj) = all_mids.mids.as_object() {
                                        for (coin, mid_price_value) in mids_obj {
                                            if !symbols_set.is_empty() && !symbols_set.contains(coin) {
                                                continue;
                                            }

                                            if let Some(mid_price_str) = mid_price_value.as_str() {
                                                match Decimal::from_str(mid_price_str) {
                                                    Ok(mid_price) => {
                                                        // Create a basic ticker from mid price
                                                        let ticker = Ticker {
                                                            symbol: coin.clone(),
                                                            last_price: mid_price,
                                                            mark_price: mid_price,
                                                            index_price: mid_price,
                                                            best_bid_price: Decimal::ZERO,
                                                            best_bid_qty: Decimal::ZERO,
                                                            best_ask_price: Decimal::ZERO,
                                                            best_ask_qty: Decimal::ZERO,
                                                            volume_24h: Decimal::ZERO,
                                                            turnover_24h: Decimal::ZERO,
                                                            open_interest: Decimal::ZERO,
                                                            open_interest_notional: Decimal::ZERO,
                                                            price_change_24h: Decimal::ZERO,
                                                            price_change_pct: Decimal::ZERO,
                                                            high_price_24h: Decimal::ZERO,
                                                            low_price_24h: Decimal::ZERO,
                                                            timestamp: Utc::now(),
                                                        };
                                                        yield Ok(ticker);
                                                    }
                                                    Err(e) => {
                                                        tracing::debug!("Failed to parse mid price: {}", e);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                HyperliquidWsResponse::SubscriptionResponse(_) => {
                                    tracing::debug!("Subscription confirmed");
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

        // Subscribe to trades for each symbol
        for symbol in &symbols {
            self.subscribe(
                &mut ws_stream,
                HyperliquidWsSubscription::Trades {
                    coin: symbol.clone(),
                },
            )
            .await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(response) = serde_json::from_str::<HyperliquidWsResponse>(&text) {
                            match response {
                                HyperliquidWsResponse::Trades(trades) => {
                                    for ws_trade in trades {
                                        match client.convert_trade(&ws_trade) {
                                            Ok(trade) => yield Ok(trade),
                                            Err(e) => yield Err(anyhow!("Failed to convert trade: {}", e)),
                                        }
                                    }
                                }
                                HyperliquidWsResponse::SubscriptionResponse(_) => {
                                    tracing::debug!("Trade subscription confirmed");
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

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to L2 book for each symbol
        for symbol in &symbols {
            self.subscribe(
                &mut ws_stream,
                HyperliquidWsSubscription::L2Book {
                    coin: symbol.clone(),
                },
            )
            .await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(response) = serde_json::from_str::<HyperliquidWsResponse>(&text) {
                            match response {
                                HyperliquidWsResponse::L2Book(ws_book) => {
                                    match client.convert_orderbook(&ws_book) {
                                        Ok(orderbook) => yield Ok(orderbook),
                                        Err(e) => yield Err(anyhow!("Failed to convert orderbook: {}", e)),
                                    }
                                }
                                HyperliquidWsResponse::SubscriptionResponse(_) => {
                                    tracing::debug!("Orderbook subscription confirmed");
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

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to requested data types for each symbol
        for symbol in &config.symbols {
            for data_type in &config.data_types {
                let subscription = match data_type {
                    StreamDataType::Ticker => HyperliquidWsSubscription::AllMids,
                    StreamDataType::Trade => HyperliquidWsSubscription::Trades {
                        coin: symbol.clone(),
                    },
                    StreamDataType::Orderbook => HyperliquidWsSubscription::L2Book {
                        coin: symbol.clone(),
                    },
                    StreamDataType::FundingRate => continue, // Hyperliquid doesn't have funding rate WebSocket
                    StreamDataType::Kline => continue, // Klines not yet implemented for Hyperliquid WebSocket
                };

                self.subscribe(&mut ws_stream, subscription).await?;
            }
        }

        let client = self.clone();
        let symbols_set: std::collections::HashSet<String> =
            config.symbols.iter().cloned().collect();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        tracing::debug!("Received message: {}", &text[..text.len().min(200)]);

                        if let Ok(response) = serde_json::from_str::<HyperliquidWsResponse>(&text) {
                            match response {
                                HyperliquidWsResponse::AllMids(all_mids) => {
                                    // Parse mids object
                                    if let Some(mids_obj) = all_mids.mids.as_object() {
                                        for (coin, mid_price_value) in mids_obj {
                                            if !symbols_set.is_empty() && !symbols_set.contains(coin) {
                                                continue;
                                            }

                                            if let Some(mid_price_str) = mid_price_value.as_str() {
                                                if let Ok(mid_price) = Decimal::from_str(mid_price_str) {
                                                    let ticker = Ticker {
                                                        symbol: coin.clone(),
                                                        last_price: mid_price,
                                                        mark_price: mid_price,
                                                        index_price: mid_price,
                                                        best_bid_price: Decimal::ZERO,
                                                        best_bid_qty: Decimal::ZERO,
                                                        best_ask_price: Decimal::ZERO,
                                                        best_ask_qty: Decimal::ZERO,
                                                        volume_24h: Decimal::ZERO,
                                                        turnover_24h: Decimal::ZERO,
                                                        open_interest: Decimal::ZERO,
                                                        open_interest_notional: Decimal::ZERO,
                                                        price_change_24h: Decimal::ZERO,
                                                        price_change_pct: Decimal::ZERO,
                                                        high_price_24h: Decimal::ZERO,
                                                        low_price_24h: Decimal::ZERO,
                                                        timestamp: Utc::now(),
                                                    };
                                                    yield Ok(StreamEvent::Ticker(ticker));
                                                }
                                            }
                                        }
                                    }
                                }
                                HyperliquidWsResponse::Trades(trades) => {
                                    for ws_trade in trades {
                                        if let Ok(trade) = client.convert_trade(&ws_trade) {
                                            yield Ok(StreamEvent::Trade(trade));
                                        }
                                    }
                                }
                                HyperliquidWsResponse::L2Book(ws_book) => {
                                    if let Ok(orderbook) = client.convert_orderbook(&ws_book) {
                                        yield Ok(StreamEvent::Orderbook(orderbook));
                                    }
                                }
                                HyperliquidWsResponse::SubscriptionResponse(_) => {
                                    tracing::debug!("Subscription confirmed");
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
