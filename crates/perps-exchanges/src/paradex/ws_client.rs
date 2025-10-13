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

const WS_BASE_URL: &str = "wss://ws.api.prod.paradex.trade/v1";

/// Paradex WebSocket streaming client
#[derive(Clone)]
pub struct ParadexWsClient {
    base_url: String,
}

impl ParadexWsClient {
    pub fn new() -> Self {
        Self {
            base_url: WS_BASE_URL.to_string(),
        }
    }

    /// Connect to WebSocket and return the stream
    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        tracing::info!("Connecting to Paradex WebSocket: {}", self.base_url);
        let (ws_stream, response) = connect_async(&self.base_url).await?;
        tracing::info!("Connected to Paradex WebSocket (status: {:?})", response.status());
        Ok(ws_stream)
    }

    /// Subscribe to a channel
    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        channel: String,
        market: Option<String>,
    ) -> Result<()> {
        let request = ParadexWsSubscribeRequest {
            method: "SUBSCRIBE".to_string(),
            params: ParadexWsSubscribeParams { channel, market },
        };
        let sub_message = serde_json::to_string(&request)?;
        tracing::debug!("Subscribing: {}", sub_message);
        ws_stream.send(Message::Text(sub_message)).await?;
        Ok(())
    }

    /// Convert Paradex market summary to our Ticker type
    fn convert_ticker(&self, summary: &ParadexMarketSummaryItem) -> Result<Ticker> {
        let last_price = Decimal::from_str(&summary.last_traded_price)?;
        let mark_price = Decimal::from_str(&summary.mark_price)?;
        let index_price = Decimal::from_str(&summary.underlying_price)?;

        // Calculate 24h price change from rate
        let price_change_rate = Decimal::from_str(&summary.price_change_rate_24h)?;
        let price_change_24h = if price_change_rate != Decimal::ZERO {
            last_price * price_change_rate / (Decimal::ONE + price_change_rate)
        } else {
            Decimal::ZERO
        };

        // Estimate high/low from price change
        let price_change_abs = price_change_24h.abs();
        let high_price_24h = last_price + price_change_abs;
        let low_price_24h = if last_price > price_change_abs {
            last_price - price_change_abs
        } else {
            Decimal::ZERO
        };

        Ok(Ticker {
            symbol: summary.symbol.clone(),
            last_price,
            mark_price,
            index_price,
            best_bid_price: Decimal::ZERO, // Not available in market summary
            best_bid_qty: Decimal::ZERO,
            best_ask_price: Decimal::ZERO,
            best_ask_qty: Decimal::ZERO,
            volume_24h: Decimal::ZERO, // Paradex provides notional volume
            turnover_24h: Decimal::from_str(&summary.volume_24h)?,
            price_change_24h,
            price_change_pct: price_change_rate,
            high_price_24h,
            low_price_24h,
            timestamp: Utc.timestamp_millis_opt(summary.created_at as i64).unwrap(),
        })
    }

    /// Convert Paradex trade to our Trade type
    fn convert_trade(&self, trade: &ParadexTradeItem) -> Result<Trade> {
        Ok(Trade {
            id: trade.timestamp.to_string(),
            symbol: trade.market.clone(),
            price: Decimal::from_str(&trade.price)?,
            quantity: Decimal::from_str(&trade.size)?,
            side: if trade.side == "buy" {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            },
            timestamp: Utc.timestamp_opt(trade.timestamp, 0).unwrap(),
        })
    }

    /// Convert Paradex orderbook to our Orderbook type
    fn convert_orderbook(&self, ob: &ParadexOrderbookSnapshot) -> Result<Orderbook> {
        let bids: Vec<OrderbookLevel> = ob
            .bids
            .iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(price)?,
                    quantity: Decimal::from_str(quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks: Vec<OrderbookLevel> = ob
            .asks
            .iter()
            .map(|(price, quantity)| {
                Ok(OrderbookLevel {
                    price: Decimal::from_str(price)?,
                    quantity: Decimal::from_str(quantity)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Orderbook {
            symbol: ob.market.clone(),
            bids,
            asks,
            timestamp: Utc.timestamp_millis_opt(ob.last_updated_at as i64).unwrap(),
        })
    }

    /// Convert Paradex funding data to our FundingRate type
    fn convert_funding_rate(&self, data: &ParadexFundingDataItem) -> Result<FundingRate> {
        Ok(FundingRate {
            symbol: data.market.clone(),
            funding_rate: Decimal::from_str(&data.funding_rate)?,
            funding_time: Utc.timestamp_millis_opt(data.created_at as i64).unwrap(),
            predicted_rate: Decimal::ZERO,
            next_funding_time: Utc.timestamp_millis_opt(0).unwrap(),
            funding_interval: 0,
            funding_rate_cap_floor: Decimal::ZERO,
        })
    }
}

impl Default for ParadexWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IPerpsStream for ParadexWsClient {
    fn get_name(&self) -> &str {
        "paradex"
    }

    async fn stream_tickers(&self, symbols: Vec<String>) -> Result<DataStream<Ticker>> {
        let mut ws_stream = self.connect().await?;

        // Subscribe to market summary for each symbol
        for symbol in &symbols {
            self.subscribe(&mut ws_stream, "markets_summary".to_string(), Some(symbol.clone()))
                .await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        tracing::debug!("Received message: {}", &text[..text.len().min(200)]);

                        // Check for ping
                        if text.contains("\"type\":\"ping\"") {
                            let pong = ParadexWsPong { msg_type: "pong".to_string() };
                            if let Ok(pong_msg) = serde_json::to_string(&pong) {
                                if let Err(e) = ws_stream.send(Message::Text(pong_msg)).await {
                                    tracing::error!("Failed to send pong: {}", e);
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            continue;
                        }

                        if let Ok(summary_msg) = serde_json::from_str::<ParadexWsMarketSummary>(&text) {
                            match client.convert_ticker(&summary_msg.params.data) {
                                Ok(ticker) => yield Ok(ticker),
                                Err(e) => yield Err(anyhow!("Failed to convert ticker: {}", e)),
                            }
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
            self.subscribe(&mut ws_stream, "trades".to_string(), Some(symbol.clone()))
                .await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Check for ping
                        if text.contains("\"type\":\"ping\"") {
                            let pong = ParadexWsPong { msg_type: "pong".to_string() };
                            if let Ok(pong_msg) = serde_json::to_string(&pong) {
                                if let Err(e) = ws_stream.send(Message::Text(pong_msg)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            continue;
                        }

                        if let Ok(trades_msg) = serde_json::from_str::<ParadexWsTrades>(&text) {
                            for trade in &trades_msg.params.data {
                                match client.convert_trade(trade) {
                                    Ok(trade) => yield Ok(trade),
                                    Err(e) => yield Err(anyhow!("Failed to convert trade: {}", e)),
                                }
                            }
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

        // Subscribe to order book for each symbol
        for symbol in &symbols {
            self.subscribe(&mut ws_stream, "order_book".to_string(), Some(symbol.clone()))
                .await?;
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Check for ping
                        if text.contains("\"type\":\"ping\"") {
                            let pong = ParadexWsPong { msg_type: "pong".to_string() };
                            if let Ok(pong_msg) = serde_json::to_string(&pong) {
                                if let Err(e) = ws_stream.send(Message::Text(pong_msg)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            continue;
                        }

                        if let Ok(ob_msg) = serde_json::from_str::<ParadexWsOrderbook>(&text) {
                            match client.convert_orderbook(&ob_msg.params.data) {
                                Ok(orderbook) => yield Ok(orderbook),
                                Err(e) => yield Err(anyhow!("Failed to convert orderbook: {}", e)),
                            }
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

        // Subscribe to requested topics for each symbol
        for symbol in &config.symbols {
            for data_type in &config.data_types {
                let channel = match data_type {
                    StreamDataType::Ticker => "markets_summary",
                    StreamDataType::Trade => "trades",
                    StreamDataType::Orderbook => "order_book",
                    StreamDataType::FundingRate => "funding_data",
                    StreamDataType::Kline => continue, // Klines not yet implemented for Paradex WebSocket
                };
                self.subscribe(&mut ws_stream, channel.to_string(), Some(symbol.clone()))
                    .await?;
            }
        }

        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Check for ping
                        if text.contains("\"type\":\"ping\"") {
                            let pong = ParadexWsPong { msg_type: "pong".to_string() };
                            if let Ok(pong_msg) = serde_json::to_string(&pong) {
                                if let Err(e) = ws_stream.send(Message::Text(pong_msg)).await {
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            continue;
                        }

                        // Try to parse as different message types
                        if let Ok(summary_msg) = serde_json::from_str::<ParadexWsMarketSummary>(&text) {
                            if let Ok(ticker) = client.convert_ticker(&summary_msg.params.data) {
                                yield Ok(StreamEvent::Ticker(ticker));
                            }
                        } else if let Ok(trades_msg) = serde_json::from_str::<ParadexWsTrades>(&text) {
                            for trade in &trades_msg.params.data {
                                if let Ok(trade) = client.convert_trade(trade) {
                                    yield Ok(StreamEvent::Trade(trade));
                                }
                            }
                        } else if let Ok(ob_msg) = serde_json::from_str::<ParadexWsOrderbook>(&text) {
                            if let Ok(orderbook) = client.convert_orderbook(&ob_msg.params.data) {
                                yield Ok(StreamEvent::Orderbook(orderbook));
                            }
                        } else if let Ok(funding_msg) = serde_json::from_str::<ParadexWsFundingData>(&text) {
                            if let Ok(funding_rate) = client.convert_funding_rate(&funding_msg.params.data) {
                                yield Ok(StreamEvent::FundingRate(funding_rate));
                            }
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
