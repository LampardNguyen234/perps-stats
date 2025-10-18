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

const WS_BASE_URL: &str = "wss://fstream.binance.com";

/// Binance WebSocket streaming client
#[derive(Clone)]
pub struct BinanceWsClient {
    base_url: String,
}

impl BinanceWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
        }
    }

    /// Create a WebSocket stream URL for given symbols and stream type
    /// Uses combined streams endpoint for multiple streams
    fn build_stream_url(&self, symbols: &[String], stream_type: &str) -> Result<Url> {
        let streams: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@{}", s.to_lowercase(), stream_type))
            .collect();

        let url = if streams.len() == 1 {
            // Single stream: wss://fstream.binance.com/ws/btcusdt@ticker
            format!("{}/ws/{}", self.base_url, streams[0])
        } else {
            // Multiple streams: wss://fstream.binance.com/stream?streams=btcusdt@ticker/ethusdt@ticker
            format!("{}/stream?streams={}", self.base_url, streams.join("/"))
        };

        Url::parse(&url).map_err(|e| anyhow!("Failed to parse WebSocket URL: {}", e))
    }

    /// Connect to WebSocket and return the stream
    async fn connect(&self, url: Url) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Binance WebSocket: {}", url);
        let (ws_stream, response) = connect_async(url.as_str()).await?;
        tracing::info!(
            "Connected to Binance WebSocket (status: {:?})",
            response.status()
        );
        Ok(ws_stream)
    }

    /// Check if URL uses combined streams endpoint
    fn is_combined_stream(&self, url: &Url) -> bool {
        url.path().contains("/stream")
    }

    /// Convert Binance ticker to our Ticker type
    /// Note: The 24hrTicker stream doesn't include best bid/ask. Use bookTicker stream for real-time BBO.
    fn convert_ticker(&self, ws_ticker: &BinanceWsTicker) -> Result<Ticker> {
        Ok(Ticker {
            symbol: ws_ticker.symbol.clone(),
            last_price: Decimal::from_str(&ws_ticker.close_price)?,
            mark_price: Decimal::from_str(&ws_ticker.close_price)?, // 24hrTicker doesn't provide mark price
            index_price: Decimal::from_str(&ws_ticker.close_price)?, // Using last price as fallback
            // 24hrTicker stream doesn't include bid/ask - use bookTicker stream for real-time BBO
            best_bid_price: Decimal::ZERO,
            best_bid_qty: Decimal::ZERO,
            best_ask_price: Decimal::ZERO,
            best_ask_qty: Decimal::ZERO,
            volume_24h: Decimal::from_str(&ws_ticker.volume)?,
            turnover_24h: Decimal::from_str(&ws_ticker.quote_volume)?,
            open_interest: Decimal::ZERO,
            open_interest_notional: Decimal::ZERO,
            price_change_24h: Decimal::from_str(&ws_ticker.price_change)?,
            price_change_pct: Decimal::from_str(&ws_ticker.price_change_percent)?
                / Decimal::from(100),
            high_price_24h: Decimal::from_str(&ws_ticker.high_price)?,
            low_price_24h: Decimal::from_str(&ws_ticker.low_price)?,
            timestamp: Utc.timestamp_millis_opt(ws_ticker.event_time).unwrap(),
        })
    }

    /// Convert Binance trade to our Trade type
    fn convert_trade(&self, ws_trade: &BinanceWsTrade) -> Result<Trade> {
        Ok(Trade {
            id: ws_trade.aggregate_trade_id.to_string(),
            symbol: ws_trade.symbol.clone(),
            price: Decimal::from_str(&ws_trade.price)?,
            quantity: Decimal::from_str(&ws_trade.quantity)?,
            side: if ws_trade.is_buyer_maker {
                OrderSide::Sell
            } else {
                OrderSide::Buy
            },
            timestamp: Utc.timestamp_millis_opt(ws_trade.trade_time).unwrap(),
        })
    }

    /// Convert Binance depth update to our Orderbook type
    fn convert_orderbook(&self, ws_depth: &BinanceWsDepthUpdate) -> Result<Orderbook> {
        use super::conversions::normalize_symbol;

        let bids: Vec<OrderbookLevel> = ws_depth
            .bids
            .iter()
            .map(|(price, qty)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(price)?,
                    quantity: Decimal::from_str(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks: Vec<OrderbookLevel> = ws_depth
            .asks
            .iter()
            .map(|(price, qty)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(price)?,
                    quantity: Decimal::from_str(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            // Normalize symbol: BTCUSDT -> BTC
            symbol: normalize_symbol(&ws_depth.symbol),
            bids,
            asks,
            timestamp: Utc.timestamp_millis_opt(ws_depth.event_time).unwrap(),
        })
    }

    /// Convert Binance depth update to DepthUpdate for OrderbookStreamer trait
    fn convert_to_depth_update(&self, ws_depth: &BinanceWsDepthUpdate) -> Result<DepthUpdate> {
        let bids: Vec<OrderbookLevel> = ws_depth
            .bids
            .iter()
            .map(|(price, qty)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(price)?,
                    quantity: Decimal::from_str(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks: Vec<OrderbookLevel> = ws_depth
            .asks
            .iter()
            .map(|(price, qty)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(price)?,
                    quantity: Decimal::from_str(qty)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(DepthUpdate {
            symbol: ws_depth.symbol.clone(), // Keep symbol as-is (BTCUSDT format) to match subscription
            first_update_id: ws_depth.first_update_id as u64,
            final_update_id: ws_depth.final_update_id as u64,
            previous_id: ws_depth.previous_update_id as u64,
            bids,
            asks,
        })
    }

    /// Convert Binance depth update to orderbook with update IDs
    /// Returns (symbol, first_update_id, final_update_id, previous_id, bids, asks)
    /// DEPRECATED: Use convert_to_depth_update instead
    pub fn convert_depth_update(
        &self,
        ws_depth: &BinanceWsDepthUpdate,
    ) -> Result<(
        String,
        u64,
        u64,
        u64,
        Vec<OrderbookLevel>,
        Vec<OrderbookLevel>,
    )> {
        let depth_update = self.convert_to_depth_update(ws_depth)?;
        Ok((
            depth_update.symbol,
            depth_update.first_update_id,
            depth_update.final_update_id,
            depth_update.previous_id,
            depth_update.bids,
            depth_update.asks,
        ))
    }

    /// Convert Binance mark price update to our FundingRate type
    fn convert_funding_rate(&self, ws_mark_price: &BinanceWsMarkPrice) -> Result<FundingRate> {
        let next_funding = Utc
            .timestamp_millis_opt(ws_mark_price.next_funding_time)
            .unwrap();
        Ok(FundingRate {
            symbol: ws_mark_price.symbol.clone(),
            funding_rate: Decimal::from_str(&ws_mark_price.funding_rate)?,
            // Binance doesn't provide predicted funding rate in WebSocket stream
            // Use current rate as predicted rate
            predicted_rate: Decimal::from_str(&ws_mark_price.funding_rate)?,
            funding_time: next_funding,
            next_funding_time: next_funding,
            funding_interval: 8, // Binance perpetual futures have 8-hour funding intervals
            funding_rate_cap_floor: Decimal::ZERO, // Not provided in WebSocket stream
        })
    }
}

impl Default for BinanceWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for BinanceWsClient {
    fn get_name(&self) -> &str {
        "binance"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let url = self.build_stream_url(&symbols, "ticker")?;
        let is_combined = self.is_combined_stream(&url);
        let mut ws_stream = self.connect(url).await?;
        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Handle combined stream wrapper
                        let ticker_result = if is_combined {
                            match serde_json::from_str::<BinanceWsCombinedStream>(&text) {
                                Ok(combined) => {
                                    serde_json::from_value::<BinanceWsTicker>(combined.data)
                                        .map_err(|e| anyhow!("Failed to parse ticker data: {}", e))
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to parse combined stream: {}", e);
                                    continue;
                                }
                            }
                        } else {
                            serde_json::from_str::<BinanceWsTicker>(&text)
                                .map_err(|e| anyhow!("Failed to parse ticker: {}", e))
                        };

                        match ticker_result {
                            Ok(ws_ticker) => {
                                match client.convert_ticker(&ws_ticker) {
                                    Ok(ticker) => yield Ok(ticker),
                                    Err(e) => yield Err(anyhow!("Failed to convert ticker: {}", e)),
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Skipping message: {}", e);
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        // Respond to ping with pong
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            tracing::error!("Failed to send pong: {}", e);
                            yield Err(anyhow!("Failed to send pong: {}", e));
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => {
                        // Received pong response
                        tracing::debug!("Received pong");
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {
                        tracing::debug!("Received other message type");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_trades(&self, symbols: Vec<String>) -> Result<DataStream<Trade>> {
        let url = self.build_stream_url(&symbols, "aggTrade")?;
        let is_combined = self.is_combined_stream(&url);
        let mut ws_stream = self.connect(url).await?;
        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let trade_result = if is_combined {
                            match serde_json::from_str::<BinanceWsCombinedStream>(&text) {
                                Ok(combined) => {
                                    serde_json::from_value::<BinanceWsTrade>(combined.data)
                                        .map_err(|e| anyhow!("Failed to parse trade data: {}", e))
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to parse combined stream: {}", e);
                                    continue;
                                }
                            }
                        } else {
                            serde_json::from_str::<BinanceWsTrade>(&text)
                                .map_err(|e| anyhow!("Failed to parse trade: {}", e))
                        };

                        match trade_result {
                            Ok(ws_trade) => {
                                match client.convert_trade(&ws_trade) {
                                    Ok(trade) => yield Ok(trade),
                                    Err(e) => yield Err(anyhow!("Failed to convert trade: {}", e)),
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Skipping message: {}", e);
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
                    Ok(Message::Pong(_)) => {
                        tracing::debug!("Received pong");
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {
                        tracing::debug!("Received other message type");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_orderbooks(&self, symbols: Vec<String>) -> Result<DataStream<Orderbook>> {
        let url = self.build_stream_url(&symbols, "depth@500ms")?;
        let is_combined = self.is_combined_stream(&url);
        let mut ws_stream = self.connect(url).await?;
        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let depth_result = if is_combined {
                            match serde_json::from_str::<BinanceWsCombinedStream>(&text) {
                                Ok(combined) => {
                                    serde_json::from_value::<BinanceWsDepthUpdate>(combined.data)
                                        .map_err(|e| anyhow!("Failed to parse depth data: {}", e))
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to parse combined stream: {}", e);
                                    continue;
                                }
                            }
                        } else {
                            serde_json::from_str::<BinanceWsDepthUpdate>(&text)
                                .map_err(|e| anyhow!("Failed to parse depth: {}", e))
                        };

                        match depth_result {
                            Ok(ws_depth) => {
                                match client.convert_orderbook(&ws_depth) {
                                    Ok(orderbook) => yield Ok(orderbook),
                                    Err(e) => yield Err(anyhow!("Failed to convert orderbook: {}", e)),
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Skipping message: {}", e);
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
                    Ok(Message::Pong(_)) => {
                        tracing::debug!("Received pong");
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {
                        tracing::debug!("Received other message type");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn stream_multi(&self, config: StreamConfig) -> Result<DataStream<StreamEvent>> {
        let mut streams = Vec::new();

        for data_type in &config.data_types {
            let stream_suffix = match data_type {
                StreamDataType::Ticker => "ticker",
                StreamDataType::Trade => "aggTrade",
                StreamDataType::Orderbook => "depth@100ms",
                StreamDataType::FundingRate => "markPrice", // Mark price stream includes funding rate
                StreamDataType::Kline => continue, // Klines not yet implemented for Binance WebSocket
            };

            for symbol in &config.symbols {
                streams.push(format!("{}@{}", symbol.to_lowercase(), stream_suffix));
            }
        }

        // Use correct URL format based on number of streams
        let url = if streams.len() == 1 {
            format!("{}/ws/{}", self.base_url, streams[0])
        } else {
            format!("{}/stream?streams={}", self.base_url, streams.join("/"))
        };
        let url = Url::parse(&url)?;
        let is_combined = self.is_combined_stream(&url);
        let mut ws_stream = self.connect(url).await?;
        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        tracing::debug!("Received text message: {}", &text[..text.len().min(200)]);

                        // Parse combined stream wrapper if needed
                        let data_json = if is_combined {
                            match serde_json::from_str::<BinanceWsCombinedStream>(&text) {
                                Ok(combined) => combined.data,
                                Err(e) => {
                                    tracing::debug!("Failed to parse combined stream: {}. Message: {}", e, &text[..text.len().min(300)]);
                                    continue;
                                }
                            }
                        } else {
                            match serde_json::from_str(&text) {
                                Ok(v) => v,
                                Err(e) => {
                                    tracing::debug!("Failed to parse message: {}. Message: {}", e, &text[..text.len().min(300)]);
                                    continue;
                                }
                            }
                        };

                        // Try to parse as different event types
                        if let Ok(ws_ticker) = serde_json::from_value::<BinanceWsTicker>(data_json.clone()) {
                            match client.convert_ticker(&ws_ticker) {
                                Ok(ticker) => {
                                    yield Ok(StreamEvent::Ticker(ticker));
                                    continue;
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to convert ticker: {}", e);
                                }
                            }
                        }

                        if let Ok(ws_trade) = serde_json::from_value::<BinanceWsTrade>(data_json.clone()) {
                            match client.convert_trade(&ws_trade) {
                                Ok(trade) => {
                                    yield Ok(StreamEvent::Trade(trade));
                                    continue;
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to convert trade: {}", e);
                                }
                            }
                        }

                        if let Ok(ws_depth) = serde_json::from_value::<BinanceWsDepthUpdate>(data_json.clone()) {
                            match client.convert_orderbook(&ws_depth) {
                                Ok(orderbook) => {
                                    yield Ok(StreamEvent::Orderbook(orderbook));
                                    continue;
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to convert orderbook: {}", e);
                                }
                            }
                        }

                        if let Ok(ws_mark_price) = serde_json::from_value::<BinanceWsMarkPrice>(data_json) {
                            match client.convert_funding_rate(&ws_mark_price) {
                                Ok(funding_rate) => {
                                    yield Ok(StreamEvent::FundingRate(funding_rate));
                                    continue;
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to convert funding rate: {}", e);
                                }
                            }
                        }

                        tracing::debug!("Unknown message type or failed to parse all variants");
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            tracing::error!("Failed to send pong: {}", e);
                            yield Err(anyhow!("Failed to send pong: {}", e));
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => {
                        tracing::debug!("Received pong");
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    Ok(other) => {
                        match other {
                            Message::Binary(data) => {
                                tracing::debug!("Received binary message ({} bytes)", data.len());
                            }
                            Message::Frame(_) => {
                                tracing::debug!("Received frame message");
                            }
                            _ => {
                                tracing::debug!("Received unexpected message type: {:?}", other);
                            }
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Implementation of OrderbookStreamer trait for optimized orderbook streaming
#[async_trait]
impl OrderbookStreamer for BinanceWsClient {
    async fn stream_depth_updates(&self, symbols: Vec<String>) -> Result<DepthUpdateStream> {
        let url = self.build_stream_url(&symbols, "depth@500ms")?;
        let is_combined = self.is_combined_stream(&url);
        let mut ws_stream = self.connect(url).await?;
        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let depth_result = if is_combined {
                            match serde_json::from_str::<BinanceWsCombinedStream>(&text) {
                                Ok(combined) => {
                                    serde_json::from_value::<BinanceWsDepthUpdate>(combined.data)
                                        .map_err(|e| anyhow!("Failed to parse depth data: {}", e))
                                }
                                Err(e) => {
                                    tracing::debug!("Failed to parse combined stream: {}", e);
                                    continue;
                                }
                            }
                        } else {
                            serde_json::from_str::<BinanceWsDepthUpdate>(&text)
                                .map_err(|e| anyhow!("Failed to parse depth: {}", e))
                        };

                        match depth_result {
                            Ok(ws_depth) => {
                                match client.convert_to_depth_update(&ws_depth) {
                                    Ok(depth_update) => yield Ok(depth_update),
                                    Err(e) => yield Err(anyhow!("Failed to convert depth update: {}", e)),
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Skipping message: {}", e);
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
                    Ok(Message::Pong(_)) => {
                        tracing::debug!("Received pong");
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {
                        tracing::debug!("Received other message type");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    fn is_incremental_delta(&self) -> bool {
        false // Binance uses full price updates
    }

    fn exchange_name(&self) -> &str {
        "binance"
    }

    fn ws_base_url(&self) -> &str {
        WS_BASE_URL
    }
}
