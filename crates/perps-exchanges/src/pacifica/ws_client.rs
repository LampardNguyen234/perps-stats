use super::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::Decimal;
use std::str::FromStr;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

const WS_BASE_URL: &str = "wss://ws.pacifica.fi/ws";

/// Pacifica WebSocket streaming client
#[derive(Clone)]
pub struct PacificaWsClient {
    ws_base_url: String,
}

impl PacificaWsClient {
    pub fn new() -> Self {
        Self {
            ws_base_url: WS_BASE_URL.to_string(),
        }
    }

    /// Connect to Pacifica WebSocket endpoint
    async fn connect(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let ws_url = &self.ws_base_url;
        tracing::info!("[Pacifica] Connecting to WebSocket: {}", ws_url);

        // Build WebSocket request with User-Agent header
        // Pacifica requires User-Agent for Cloudflare protection
        let request = ws_url.as_str().into_client_request()?;

        let (ws_stream, response) = connect_async(request).await?;

        tracing::info!(
            "[Pacifica] Connected to WebSocket (status: {})",
            response.status()
        );

        Ok(ws_stream)
    }

    /// Subscribe to orderbook channel with agg_level=10
    async fn subscribe(
        &self,
        ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        symbol: String,
        agg_level: u32,
    ) -> Result<()> {
        let sub_msg = PacificaWsSubscribeRequest {
            method: "subscribe".to_string(),
            params: PacificaWsSubscribeParams {
                source: "book".to_string(),
                symbol: symbol.clone(),
                agg_level,
            },
        };

        let sub_json = serde_json::to_string(&sub_msg)?;
        tracing::debug!("[Pacifica] Subscribing: {}", sub_json);

        ws_stream.send(Message::Text(sub_json)).await?;

        tracing::info!(
            "[Pacifica] Subscribed to orderbook for {} (agg_level={})",
            symbol,
            agg_level
        );

        Ok(())
    }

    /// Convert Pacifica WebSocket orderbook update to DepthUpdate
    fn convert_to_depth_update(&self, msg: PacificaWsOrderbookUpdate) -> Result<DepthUpdate> {
        // Parse bids (index 0)
        let bids = if msg.levels.is_empty() {
            Vec::new()
        } else {
            msg.levels[0]
                .iter()
                .map(|level| {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level.price)?,
                        quantity: Decimal::from_str(&level.amount)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };

        // Parse asks (index 1)
        let asks = if msg.levels.len() < 2 {
            Vec::new()
        } else {
            msg.levels[1]
                .iter()
                .map(|level| {
                    Ok(OrderbookLevel {
                        price: Decimal::from_str(&level.price)?,
                        quantity: Decimal::from_str(&level.amount)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };

        // Use timestamp as sequence if no sequence number provided
        let sequence = msg.sequence.unwrap_or(msg.timestamp) as u64;

        Ok(DepthUpdate {
            symbol: msg.symbol,
            first_update_id: sequence,
            final_update_id: sequence,
            previous_id: 0, // Gap detection mode
            bids,
            asks,
            is_snapshot: false, // Pacifica sends full snapshots, not incremental deltas
        })
    }
}

impl Default for PacificaWsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OrderbookStreamer for PacificaWsClient {
    async fn stream_depth_updates(&self, symbols: Vec<String>) -> Result<DepthUpdateStream> {
        // Pacifica supports one symbol per WebSocket connection
        if symbols.len() != 1 {
            return Err(anyhow!(
                "Pacifica only supports one symbol per WebSocket connection (got {} symbols). Each symbol requires a separate connection.",
                symbols.len()
            ));
        }

        let symbol = &symbols[0];
        let mut ws_stream = self.connect().await?;

        // Subscribe to orderbook with agg_level=10 as specified
        self.subscribe(&mut ws_stream, symbol.clone(), 10).await?;

        let symbol_clone = symbol.clone();
        let client_clone = self.clone();

        let stream = async_stream::stream! {
            // Set up ping interval (30 seconds)
            let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));
            ping_interval.tick().await; // Skip first immediate tick
            let mut ping_counter: u64 = 0;

            loop {
                tokio::select! {
                    // Handle incoming messages
                    msg = ws_stream.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                tracing::trace!("[Pacifica] Received message: {}", text);

                                match serde_json::from_str::<PacificaWsResponse>(&text) {
                                    Ok(response) => {
                                        tracing::trace!("[Pacifica] Parsed channel: {}", response.channel);
                                        match response.channel.as_str() {
                                        "subscribe" => {
                                            tracing::info!("[Pacifica] Subscription confirmed for {}", symbol_clone);
                                        }
                                        "book" => {
                                            if let Some(data) = response.data {
                                                match serde_json::from_value::<PacificaWsOrderbookUpdate>(data.clone()) {
                                                    Ok(orderbook_update) => {
                                                        match client_clone.convert_to_depth_update(orderbook_update) {
                                                            Ok(depth_update) => {
                                                                tracing::trace!(
                                                                    "[Pacifica] Orderbook update for {}: {} bids, {} asks",
                                                                    symbol_clone,
                                                                    depth_update.bids.len(),
                                                                    depth_update.asks.len()
                                                                );
                                                                yield Ok(depth_update);
                                                            }
                                                            Err(e) => {
                                                                tracing::warn!("[Pacifica] Failed to convert depth update: {}", e);
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            "[Pacifica] Failed to parse orderbook update: {}. Raw data: {}",
                                                            e,
                                                            serde_json::to_string(&data).unwrap_or_else(|_| "failed to serialize".to_string())
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        "pong" => {
                                            tracing::trace!("[Pacifica] Pong received");
                                        }
                                        "error" => {
                                            let error_msg = response.message.unwrap_or_else(|| "Unknown error".to_string());
                                            tracing::error!("[Pacifica] WebSocket error: {}", error_msg);
                                            yield Err(anyhow!("WebSocket error: {}", error_msg));
                                            break;
                                        }
                                        _ => {
                                            tracing::warn!("[Pacifica] Unknown channel: {}", response.channel);
                                        }
                                    }
                                    }
                                    Err(e) => {
                                        tracing::warn!("[Pacifica] Failed to parse WebSocket message: {}. Raw: {}", e, text);
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                tracing::trace!("[Pacifica] Ping received from server, sending pong");
                                if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                                    tracing::error!("[Pacifica] Failed to send pong: {}", e);
                                    yield Err(anyhow!("Failed to send pong: {}", e));
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(frame))) => {
                                tracing::info!("[Pacifica] WebSocket closed: {:?}", frame);
                                break;
                            }
                            Some(Err(e)) => {
                                tracing::error!("[Pacifica] WebSocket error: {}", e);
                                yield Err(anyhow!("WebSocket error: {}", e));
                                break;
                            }
                            None => {
                                tracing::info!("[Pacifica] WebSocket stream ended");
                                break;
                            }
                            _ => {}
                        }
                    }

                    // Send periodic ping messages (every 30 seconds)
                    _ = ping_interval.tick() => {
                        ping_counter += 1;
                        let ping_msg = PacificaWsPingRequest {
                            msg_type: "ping".to_string(),
                        };

                        match serde_json::to_string(&ping_msg) {
                            Ok(ping_json) => {
                                tracing::debug!("[Pacifica] Sending ping #{}", ping_counter);
                                if let Err(e) = ws_stream.send(Message::Text(ping_json)).await {
                                    tracing::error!("[Pacifica] Failed to send ping: {}", e);
                                    yield Err(anyhow!("Failed to send ping: {}", e));
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::error!("[Pacifica] Failed to serialize ping message: {}", e);
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
        false
    }

    fn exchange_name(&self) -> &str {
        "pacifica"
    }

    fn ws_base_url(&self) -> &str {
        &self.ws_base_url
    }

    fn connection_config(&self) -> ConnectionConfig {
        ConnectionConfig {
            ping_interval: std::time::Duration::from_secs(30),
            pong_timeout: std::time::Duration::from_secs(10),
            reconnect_delay: std::time::Duration::from_secs(2),
            max_reconnect_attempts: 10,
        }
    }
}
