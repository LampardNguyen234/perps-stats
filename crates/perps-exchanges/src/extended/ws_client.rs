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
        tracing::trace!("convert_orderbook: {:?}", ws_orderbook);

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

        // DELTA messages carry both `q` (the change) and `c` (the resulting absolute
        // quantity at that price). We always use the absolute value: `c` for DELTA, `q`
        // for SNAPSHOT (which has no `c` since `q` is already absolute there). This
        // reconstructs every level from ground truth on each message instead of summing
        // `q` over time, which would drift permanently the moment a single message is
        // lost — Extended gives no sequence/gap signal to detect that (previous_id is
        // always 0 below).
        let to_levels = |raw: &[ExtendedWsLevel]| -> Result<Vec<OrderbookLevel>> {
            raw.iter()
                .map(|level| {
                    let price = Decimal::from_str(&level.price)?;
                    let quantity_str = if is_snapshot {
                        &level.quantity
                    } else {
                        level.cumulative_quantity.as_ref().unwrap_or(&level.quantity)
                    };
                    let quantity = Decimal::from_str(quantity_str)?;

                    Ok(OrderbookLevel { price, quantity })
                })
                .collect()
        };

        let bids = to_levels(&ws_orderbook.data.bids)?;
        let asks = to_levels(&ws_orderbook.data.asks)?;

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

    /// Subscribe to the orderbook stream for a specific market, or every market at once
    /// if `market` is `None`.
    ///
    /// Extended has no per-market subscribe frame: the market is picked by URL path.
    /// Omitting it (`GET .../v1/orderbooks`, no trailing segment) streams all markets
    /// multiplexed on the one connection, each message tagged with `data.m`.
    ///
    /// Extended requires a User-Agent header to accept WebSocket connections.
    /// Without it, the server returns 403 Forbidden.
    pub async fn subscribe_orderbook(
        &self,
        market: Option<&str>,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        const STREAM_BASE: &str =
            "wss://api.starknet.extended.exchange/stream.extended.exchange/v1/orderbooks";
        let market_url = match market {
            Some(m) => format!("{STREAM_BASE}/{m}"),
            None => STREAM_BASE.to_string(),
        };

        // Build WebSocket request with User-Agent header
        // Extended requires User-Agent or returns 403 Forbidden
        let mut request = market_url.as_str().into_client_request()?;
        request
            .headers_mut()
            .insert("User-Agent", USER_AGENT.parse().unwrap());

        tracing::debug!(
            "Connecting to Extended WebSocket for {} with User-Agent: {}",
            market.unwrap_or("ALL markets"),
            USER_AGENT
        );

        let (ws_stream, response) = connect_async(request).await?;

        tracing::info!(
            "✓ Extended WebSocket connected: status={}, market={}, url={}",
            response.status(),
            market.unwrap_or("ALL"),
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
        if symbols.is_empty() {
            return Err(anyhow::anyhow!(
                "stream_depth_updates called with no symbols"
            ));
        }

        // Extended has no per-market subscribe frame, so one connection with no market
        // path streams every market. Any number of requested symbols fit on this single
        // connection — we just filter client-side to the ones we were asked for.
        let wanted: std::collections::HashSet<String> = symbols.into_iter().collect();

        let mut ws_stream = self.subscribe_orderbook(None).await?;
        let client = self.clone();

        let stream = async_stream::stream! {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ExtendedWsOrderbook>(&text) {
                            Ok(ws_orderbook) => {
                                if !wanted.contains(&ws_orderbook.data.market) {
                                    continue;
                                }
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
        // We use Extended's `c` field (absolute resulting quantity) for DELTA messages
        // and `q` (already absolute) for SNAPSHOT messages — see convert_to_depth_update.
        // Every level is therefore an absolute replacement, never accumulated.
        false
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
