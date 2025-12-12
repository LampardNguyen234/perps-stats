use crate::extended::ws_types::*;
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use perps_core::streaming::*;
use perps_core::types::OrderbookLevel;
use rust_decimal::Decimal;
use std::str::FromStr;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const USER_AGENT: &str = "Mozilla/5.0 (perps-stats/0.1.0)";

/// WebSocket client for Extended Exchange
#[derive(Clone)]
pub struct ExtendedWsClient;

impl ExtendedWsClient {
    pub fn new() -> Self {
        Self
    }

    /// Convert Extended WebSocket orderbook update to structured data for OrderbookManager
    /// Returns: (symbol, is_snapshot, bids, asks, sequence_number)
    ///
    /// Extended's sequence number behavior:
    /// - Extended uses milliseasing
    /// - Each update increments by ~1-10ms depecond timestamps as sequence numbers (not simple 1, 2, 3...)
    //     /// - Sequence numbers are monotonically incrending on message batching
    /// - Out-of-order sequences should trigger reconnection (OrderbookManager handles this)
    pub fn convert_orderbook_update(
        &self,
        ws_orderbook: &ExtendedWsOrderbook,
    ) -> Result<(String, bool, Vec<OrderbookLevel>, Vec<OrderbookLevel>, u64)> {
        let is_snapshot = ws_orderbook.data.update_type == "SNAPSHOT";
        tracing::info!("convert_orderbook: {:?}", ws_orderbook);

        let bids: Vec<OrderbookLevel> = ws_orderbook
            .data
            .bids
            .iter()
            .map(|level| {
                let price = Decimal::from_str(&level.price)?;
                let quantity = Decimal::from_str(&level.quantity)?;

                Ok(OrderbookLevel { price, quantity })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks: Vec<OrderbookLevel> = ws_orderbook
            .data
            .asks
            .iter()
            .map(|level| {
                let price = Decimal::from_str(&level.price)?;
                let quantity = Decimal::from_str(&level.quantity)?;

                Ok(OrderbookLevel { price, quantity })
            })
            .collect::<Result<Vec<_>>>()?;

        // Normalize symbol from Extended format (BTC-USD) to global format (BTC)
        let symbol = ws_orderbook
            .data
            .market
            .split('-')
            .next()
            .unwrap_or(&ws_orderbook.data.market)
            .to_string();

        // Extended provides monotonic sequence numbers
        // Sequence 1 = first snapshot, subsequent deltas increment from there
        // Fall back to timestamp if sequence is missing (shouldn't happen)
        let sequence = ws_orderbook.data.sequence.unwrap_or_else(|| {
            ws_orderbook.data.timestamp.unwrap_or_else(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64
            })
        });

        Ok((symbol, is_snapshot, bids, asks, sequence as u64))
    }

    /// Convert Extended WebSocket orderbook update to DepthUpdate for OrderbookStreamer trait
    ///
    /// Extended uses sequence numbers for ordering, not first/final update IDs.
    /// We use sequence as both first and final update ID, with previous_id=0 (gap detection mode).
    fn convert_to_depth_update(&self, ws_orderbook: &ExtendedWsOrderbook) -> Result<DepthUpdate> {
        let is_snapshot = ws_orderbook.data.update_type == "SNAPSHOT";

        let bids: Vec<OrderbookLevel> = ws_orderbook
            .data
            .bids
            .iter()
            .map(|level| {
                let price = Decimal::from_str(&level.price)?;
                let quantity = Decimal::from_str(&level.quantity)?;

                Ok(OrderbookLevel { price, quantity })
            })
            .collect::<Result<Vec<_>>>()?;

        let asks: Vec<OrderbookLevel> = ws_orderbook
            .data
            .asks
            .iter()
            .map(|level| {
                let price = Decimal::from_str(&level.price)?;
                let quantity = Decimal::from_str(&level.quantity)?;

                Ok(OrderbookLevel { price, quantity })
            })
            .collect::<Result<Vec<_>>>()?;

        // Keep symbol in Extended format (BTC-USD) to match subscription
        let symbol = ws_orderbook.data.market.clone();

        // Extended provides monotonic sequence numbers
        let sequence = ws_orderbook.data.sequence.unwrap_or_else(|| {
            ws_orderbook.data.timestamp.unwrap_or_else(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64
            })
        }) as u64;

        // Log delta updates (not snapshots) for debugging bid-ask invariant violations
        if !is_snapshot && (!bids.is_empty() || !asks.is_empty()) {
            let bid_summary: Vec<String> = bids
                .iter()
                .take(3)
                .map(|l| format!("{}@{}", l.price, l.quantity))
                .collect();
            let ask_summary: Vec<String> = asks
                .iter()
                .take(3)
                .map(|l| format!("{}@{}", l.price, l.quantity))
                .collect();

            tracing::trace!(
                "[Extended Delta] {} seq={} type={}: {} bids {:?}, {} asks {:?}",
                symbol,
                sequence,
                ws_orderbook.data.update_type,
                bids.len(),
                bid_summary,
                asks.len(),
                ask_summary
            );
        }

        Ok(DepthUpdate {
            symbol,
            first_update_id: sequence,
            final_update_id: sequence,
            previous_id: 0, // Extended uses gap detection mode (previous_id not provided)
            bids,
            asks,
            is_snapshot,
        })
    }

    /// Subscribe to orderbook stream for a specific market
    ///
    /// Extended uses market-specific WebSocket URLs instead of subscription messages.
    /// URL format: wss://api.starknet.extended.exchange/stream.extended.exchange/v1/orderbooks/{MARKET}
    ///
    /// Extended requires a User-Agent header to accept WebSocket connections.
    /// Without it, the server returns 403 Forbidden.
    pub async fn subscribe_orderbook(
        &self,
        market: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        // Build market-specific WebSocket URL
        let market_url = format!(
            "wss://api.starknet.extended.exchange/stream.extended.exchange/v1/orderbooks/{}",
            market
        );

        // Build WebSocket request with User-Agent header
        // Extended requires User-Agent or returns 403 Forbidden
        let mut request = market_url.as_str().into_client_request()?;
        request
            .headers_mut()
            .insert("User-Agent", USER_AGENT.parse().unwrap());

        tracing::debug!(
            "Connecting to Extended WebSocket for {} with User-Agent: {}",
            market,
            USER_AGENT
        );

        let (ws_stream, response) = connect_async(request).await?;

        tracing::info!(
            "✓ Extended WebSocket connected: status={}, market={}, url={}",
            response.status(),
            market,
            market_url
        );

        Ok(ws_stream)
    }
}

impl Default for ExtendedWsClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Implementation of OrderbookStreamer trait for optimized orderbook streaming
#[async_trait]
impl OrderbookStreamer for ExtendedWsClient {
    async fn stream_depth_updates(&self, symbols: Vec<String>) -> Result<DepthUpdateStream> {
        if symbols.len() != 1 {
            return Err(anyhow::anyhow!(
                "Extended Exchange only supports streaming one symbol at a time (got {} symbols). Each symbol requires a separate WebSocket connection.",
                symbols.len()
            ));
        }

        let symbol = &symbols[0];
        // Symbol is already in Extended format (BTC-USD) from the client
        // No need to convert - use it directly
        let market = symbol.clone();

        let mut ws_stream = self.subscribe_orderbook(&market).await?;
        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ExtendedWsOrderbook>(&text) {
                            Ok(ws_orderbook) => {
                                match client.convert_to_depth_update(&ws_orderbook) {
                                    Ok(depth_update) => yield Ok(depth_update),
                                    Err(e) => {
                                        tracing::warn!("Failed to convert Extended orderbook update: {}", e);
                                        yield Err(anyhow::anyhow!("Failed to convert orderbook update: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Failed to parse Extended orderbook message: {}", e);
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        use futures::SinkExt;
                        if let Err(e) = ws_stream.send(Message::Pong(payload)).await {
                            tracing::error!("Failed to send pong to Extended: {}", e);
                            yield Err(anyhow::anyhow!("Failed to send pong: {}", e));
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => {
                        tracing::debug!("Received pong from Extended");
                    }
                    Ok(Message::Close(frame)) => {
                        tracing::info!("Extended WebSocket connection closed: {:?}", frame);
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Extended WebSocket error: {}", e);
                        yield Err(anyhow::anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    _ => {
                        tracing::debug!("Received other message type from Extended");
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    fn is_incremental_delta(&self) -> bool {
        true // Extended uses incremental delta updates (quantities represent changes)
    }

    fn exchange_name(&self) -> &str {
        "extended"
    }

    fn ws_base_url(&self) -> &str {
        "wss://api.starknet.extended.exchange"
    }

    fn connection_config(&self) -> ConnectionConfig {
        ConnectionConfig {
            ping_interval: std::time::Duration::from_secs(15),
            pong_timeout: std::time::Duration::from_secs(10),
            reconnect_delay: std::time::Duration::from_secs(5),
            max_reconnect_attempts: 10,
        }
    }
}
