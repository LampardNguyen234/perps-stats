use super::types::Pair;
use super::ws_types::{
    NadoWsBookDepth, NadoWsControlResponse, NadoWsPingRequest, NadoWsStream, NadoWsSubscribeRequest,
};
use super::GATEWAY_URL;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use perps_core::{
    ConnectionConfig, DepthUpdate, DepthUpdateStream, OrderbookLevel, OrderbookStreamer,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;
use yawc::{Frame, OpCode, Options, TcpWebSocket, WebSocket};

const WS_BASE_URL: &str = "wss://gateway.prod.nado.xyz/v1/subscribe";

#[derive(Debug, Default)]
struct ProductMappings {
    by_ticker: HashMap<String, u32>,
    by_product: HashMap<u32, String>,
}

impl ProductMappings {
    fn from_pairs(pairs: Vec<Pair>) -> Self {
        let mut mappings = Self::default();
        for pair in pairs {
            mappings
                .by_ticker
                .insert(pair.ticker_id.clone(), pair.product_id);
            mappings.by_product.insert(pair.product_id, pair.ticker_id);
        }
        mappings
    }
}

#[derive(Clone)]
pub struct NadoWsClient {
    ws_base_url: String,
    http: reqwest::Client,
    products: Arc<OnceCell<ProductMappings>>,
}

impl NadoWsClient {
    pub fn new() -> Self {
        Self {
            ws_base_url: WS_BASE_URL.to_string(),
            http: reqwest::Client::new(),
            products: Arc::new(OnceCell::new()),
        }
    }

    async fn product_mappings(&self) -> Result<&ProductMappings> {
        self.products
            .get_or_try_init(|| async {
                let url = format!("{GATEWAY_URL}/pairs?market=perp");
                let response = self.http.get(&url).send().await?;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    return Err(anyhow!(
                        "Nado pairs request failed with HTTP {status}: {body}"
                    ));
                }
                let pairs = response.json::<Vec<Pair>>().await?;
                Ok(ProductMappings::from_pairs(pairs))
            })
            .await
    }

    async fn connect(&self) -> Result<TcpWebSocket> {
        tracing::info!(url = %self.ws_base_url, "connecting to Nado orderbook WebSocket");
        let stream = WebSocket::connect(self.ws_base_url.parse()?)
            .with_options(
                Options::default()
                    .with_low_latency_compression()
                    .with_utf8()
                    .with_no_delay(),
            )
            .await?;
        tracing::info!("connected to Nado orderbook WebSocket");
        Ok(stream)
    }

    fn convert_book_depth(
        message: NadoWsBookDepth,
        products: &ProductMappings,
    ) -> Result<Option<DepthUpdate>> {
        if message.stream_type != "book_depth" {
            return Ok(None);
        }
        let Some(symbol) = products.by_product.get(&message.product_id) else {
            tracing::warn!(
                product_id = message.product_id,
                "nado ws: no ticker_id for product_id; dropping update"
            );
            return Ok(None);
        };

        Ok(Some(DepthUpdate {
            symbol: symbol.clone(),
            first_update_id: message
                .min_timestamp
                .parse()
                .context("invalid Nado min_timestamp")?,
            final_update_id: message
                .max_timestamp
                .parse()
                .context("invalid Nado max_timestamp")?,
            previous_id: message
                .last_max_timestamp
                .parse()
                .context("invalid Nado last_max_timestamp")?,
            bids: convert_levels(message.bids)?,
            asks: convert_levels(message.asks)?,
            is_snapshot: false,
        }))
    }
}

impl Default for NadoWsClient {
    fn default() -> Self {
        Self::new()
    }
}

fn convert_levels(levels: Vec<[String; 2]>) -> Result<Vec<OrderbookLevel>> {
    levels
        .into_iter()
        .map(|[price, quantity]| {
            Ok(OrderbookLevel {
                price: super::conversions::x18_decimal(&price)?,
                quantity: super::conversions::x18_decimal(&quantity)?,
            })
        })
        .collect()
}

fn resolve_subscriptions(symbols: Vec<String>, products: &ProductMappings) -> Vec<(String, u32)> {
    symbols
        .into_iter()
        .filter_map(|ticker_id| match products.by_ticker.get(&ticker_id) {
            Some(product_id) => Some((ticker_id, *product_id)),
            None => {
                tracing::warn!(%ticker_id, "nado ws: no product_id for ticker_id; skipping");
                None
            }
        })
        .collect()
}

fn epoch_millis() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

#[async_trait]
impl OrderbookStreamer for NadoWsClient {
    async fn stream_depth_updates(&self, symbols: Vec<String>) -> Result<DepthUpdateStream> {
        if symbols.is_empty() {
            return Err(anyhow!("Nado requires at least one symbol to stream"));
        }

        let products = self.product_mappings().await?;
        let subscriptions = resolve_subscriptions(symbols, products);
        if subscriptions.is_empty() {
            return Err(anyhow!(
                "none of the requested Nado symbols could be resolved"
            ));
        }

        let products = self.products.clone();
        let mut ws_stream = self.connect().await?;
        let mut next_id = 1_u64;
        for (ticker_id, product_id) in &subscriptions {
            let request = NadoWsSubscribeRequest {
                id: next_id,
                method: "subscribe",
                stream: NadoWsStream {
                    stream_type: "book_depth",
                    product_id: *product_id,
                },
            };
            next_id += 1;
            ws_stream
                .send(Frame::text(serde_json::to_string(&request)?))
                .await?;
            tracing::info!(%ticker_id, %product_id, "subscribed to Nado orderbook");
        }

        let stream = async_stream::stream! {
            let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
            ping_interval.tick().await;

            // Nado's REST /orderbook snapshot has no field in the same sequence space as
            // this WS stream's min/max/last_max_timestamp chain (it's just a wall-clock
            // capture time), so a fresh subscription's first frame can never satisfy
            // orderbook_manager's Rule 4 (previous_id == lastUpdateId from the snapshot) -
            // that would fail every reconnect forever, not just on a genuine gap. Zero
            // previous_id only for each symbol's first frame in this connection (trust it
            // to seed the chain); every following frame keeps its real last_max_timestamp,
            // so genuine mid-stream gaps still trigger Rule 4 and force a reconnect.
            let mut chain_seeded: HashSet<String> = HashSet::new();

            loop {
                tokio::select! {
                    incoming = ws_stream.next() => {
                        match incoming {
                            Some(frame) if matches!(frame.opcode(), OpCode::Text | OpCode::Binary) => {
                                let text = match std::str::from_utf8(frame.payload()) {
                                    Ok(text) => text.to_string(),
                                    Err(error) => {
                                        tracing::warn!(%error, "Nado WebSocket data frame was not UTF-8");
                                        continue;
                                    }
                                };
                                let value = match serde_json::from_str::<serde_json::Value>(&text) {
                                    Ok(value) => value,
                                    Err(error) => {
                                        tracing::warn!(%error, raw = %text, "failed to parse Nado WebSocket message");
                                        continue;
                                    }
                                };
                                if value.get("type").and_then(|v| v.as_str()) == Some("book_depth") {
                                    match serde_json::from_value::<NadoWsBookDepth>(value)
                                        .map_err(anyhow::Error::from)
                                        .and_then(|message| {
                                            let mappings = products.get().expect("product mappings initialized");
                                            Self::convert_book_depth(message, mappings)
                                        }) {
                                        Ok(Some(mut update)) => {
                                            if chain_seeded.insert(update.symbol.clone()) {
                                                update.previous_id = 0;
                                            }
                                            yield Ok(update)
                                        }
                                        Ok(None) => {}
                                        Err(error) => tracing::warn!(%error, raw = %text, "failed to convert Nado orderbook update"),
                                    }
                                } else if value.get("id").is_some() {
                                    match serde_json::from_value::<NadoWsControlResponse>(value) {
                                        Ok(response) => {
                                            if let Some(error) = response.error {
                                                tracing::warn!(id = response.id, %error, "Nado WebSocket request rejected");
                                            } else if response.result.get("method").and_then(|v| v.as_str()) == Some("pong") {
                                                tracing::trace!(id = response.id, "Nado WebSocket pong received");
                                            } else {
                                                tracing::debug!(id = response.id, "Nado WebSocket subscription acknowledged");
                                            }
                                        }
                                        Err(error) => tracing::warn!(%error, raw = %text, "failed to parse Nado control response"),
                                    }
                                } else {
                                    tracing::warn!(raw = %text, "unrecognized Nado WebSocket message");
                                }
                            }
                            Some(frame) if frame.opcode() == OpCode::Close => {
                                tracing::info!(code = ?frame.close_code(), "Nado WebSocket closed");
                                break;
                            }
                            None => break,
                            _ => {}
                        }
                    }
                    _ = ping_interval.tick() => {
                        let ping = NadoWsPingRequest {
                            id: next_id,
                            method: "ping",
                            client_time: epoch_millis(),
                        };
                        next_id += 1;
                        match serde_json::to_string(&ping) {
                            Ok(message) => {
                                if let Err(error) = ws_stream.send(Frame::text(message)).await {
                                    yield Err(anyhow!("failed to send Nado WebSocket ping: {error}"));
                                    break;
                                }
                            }
                            Err(error) => {
                                yield Err(anyhow!("failed to serialize Nado WebSocket ping: {error}"));
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
        "nado"
    }

    fn ws_base_url(&self) -> &str {
        &self.ws_base_url
    }

    fn connection_config(&self) -> ConnectionConfig {
        ConnectionConfig {
            ping_interval: Duration::from_secs(30),
            pong_timeout: Duration::from_secs(10),
            reconnect_delay: Duration::from_secs(2),
            max_reconnect_attempts: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn fixture_mappings() -> ProductMappings {
        ProductMappings::from_pairs(vec![
            Pair {
                product_id: 2,
                ticker_id: "BTC-PERP_USDT0".into(),
                base: "BTC".into(),
                quote: "USDT0".into(),
            },
            Pair {
                product_id: 4,
                ticker_id: "ETH-PERP_USDT0".into(),
                base: "ETH".into(),
                quote: "USDT0".into(),
            },
        ])
    }

    #[test]
    fn converts_x18_values_exactly() {
        use crate::nado::conversions::x18_decimal;
        assert_eq!(x18_decimal("77375000000000000000000").unwrap(), dec!(77375));
        assert_eq!(x18_decimal("1250000000000000000").unwrap(), dec!(1.25));
        assert_eq!(x18_decimal("1").unwrap(), dec!(0.000000000000000001));
    }

    #[test]
    fn builds_bidirectional_product_mapping() {
        let mappings = fixture_mappings();
        assert_eq!(mappings.by_ticker.get("BTC-PERP_USDT0"), Some(&2));
        assert_eq!(
            mappings.by_product.get(&2).map(String::as_str),
            Some("BTC-PERP_USDT0")
        );
        assert_eq!(mappings.by_ticker.get("SOL-PERP_USDT0"), None);
    }

    #[test]
    fn skips_symbols_missing_from_product_mapping() {
        let subscriptions = resolve_subscriptions(
            vec!["BTC-PERP_USDT0".into(), "MISSING-PERP_USDT0".into()],
            &fixture_mappings(),
        );
        assert_eq!(subscriptions, vec![("BTC-PERP_USDT0".into(), 2)]);
    }

    #[test]
    fn maps_gap_detection_fields_without_modification() {
        // convert_book_depth passes previous_id through unmodified; the first-frame-per-
        // symbol override to 0 happens one layer up, in stream_depth_updates's loop.
        let update = NadoWsClient::convert_book_depth(
            NadoWsBookDepth {
                stream_type: "book_depth".into(),
                product_id: 2,
                min_timestamp: "201".into(),
                max_timestamp: "220".into(),
                last_max_timestamp: "199".into(),
                bids: vec![[
                    "77375000000000000000000".into(),
                    "1250000000000000000".into(),
                ]],
                asks: vec![],
            },
            &fixture_mappings(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(update.symbol, "BTC-PERP_USDT0");
        assert_eq!(update.previous_id, 199);
        assert_eq!(update.first_update_id, 201);
        assert_eq!(update.final_update_id, 220);
        assert_eq!(update.bids[0].price, dec!(77375));
        assert_eq!(update.bids[0].quantity, dec!(1.25));
        assert!(!update.is_snapshot);
    }

    #[tokio::test]
    #[ignore = "requires the live Nado API"]
    async fn streams_live_book_depth() {
        let client = NadoWsClient::new();
        let mut stream = client
            .stream_depth_updates(vec!["BTC-PERP_USDT0".into()])
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(20), stream.next())
            .await
            .expect("timed out waiting for Nado orderbook update")
            .expect("Nado stream ended")
            .expect("Nado stream returned an error");

        assert_eq!(update.symbol, "BTC-PERP_USDT0");
        assert!(!update.bids.is_empty() || !update.asks.is_empty());
        assert!(update.final_update_id >= update.first_update_id);
    }
}
