use super::models::{LighterResponse, OrderBookDetailsResponse};
use super::symbols::{to_exchange_symbol, to_global_symbol};
use super::ws_types::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use perps_core::streaming::*;
use perps_core::types::*;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

const WS_BASE_URL: &str = "wss://mainnet.zklighter.elliot.ai/stream";
/// Per-market local L2 state maintained by the WS background task.
///
/// First message for a market = full snapshot → replaces maps.
/// Subsequent messages = incremental delta → apply changes, check nonce.
/// size "0" → delete that price level.
struct MarketState {
    bids: BTreeMap<Decimal, Decimal>, // price → size, ascending (iter().rev() = best-bid-first)
    asks: BTreeMap<Decimal, Decimal>, // price → size, ascending (iter() = best-ask-first)
    nonce: u64,
}

impl MarketState {
    fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            nonce: 0,
        }
    }

    fn apply_levels(
        map: &mut BTreeMap<Decimal, Decimal>,
        levels: &[LighterOrderbookLevel],
    ) -> Result<()> {
        for l in levels {
            let price = Decimal::from_str(&l.price)
                .map_err(|e| anyhow!("price parse '{}': {}", l.price, e))?;
            let size = Decimal::from_str(&l.size)
                .map_err(|e| anyhow!("size parse '{}': {}", l.size, e))?;
            if size.is_zero() {
                map.remove(&price);
            } else {
                map.insert(price, size);
            }
        }
        Ok(())
    }

    /// Populate from the first (full-snapshot) message.
    fn apply_snapshot(&mut self, ob: &LighterOrderBook) -> Result<()> {
        self.bids.clear();
        self.asks.clear();
        Self::apply_levels(&mut self.bids, &ob.bids)?;
        Self::apply_levels(&mut self.asks, &ob.asks)?;
        self.nonce = ob.nonce;
        Ok(())
    }

    /// Apply an incremental delta.  Returns `Err` on nonce gap → caller reconnects.
    fn apply_delta(&mut self, ob: &LighterOrderBook) -> Result<()> {
        if ob.begin_nonce != self.nonce {
            return Err(anyhow!(
                "nonce gap: expected {} got begin_nonce {}",
                self.nonce,
                ob.begin_nonce
            ));
        }
        Self::apply_levels(&mut self.bids, &ob.bids)?;
        Self::apply_levels(&mut self.asks, &ob.asks)?;
        self.nonce = ob.nonce;
        Ok(())
    }

    fn to_orderbook(&self, symbol: &str) -> Orderbook {
        Orderbook {
            symbol: symbol.to_string(),
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(p, q)| OrderbookLevel {
                    price: *p,
                    quantity: *q,
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(p, q)| OrderbookLevel {
                    price: *p,
                    quantity: *q,
                })
                .collect(),
            timestamp: Utc::now(),
        }
    }
}

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
        let symbol = to_exchange_symbol(symbol);
        if let Some(&market_id) = self.symbol_to_market_id.get(&symbol) {
            return Ok(market_id);
        }

        let url = "https://mainnet.zklighter.elliot.ai/api/v1/orderBookDetails";
        let response: LighterResponse<OrderBookDetailsResponse> =
            reqwest::get(url).await?.error_for_status()?.json().await?;
        if response.code != 200 {
            return Err(anyhow!("Lighter API error: {}", response.code));
        }
        self.symbol_to_market_id = response
            .data
            .order_book_details
            .into_iter()
            .filter(|d| d.is_collectable())
            .map(|d| (d.symbol, d.market_id))
            .collect();
        self.symbol_to_market_id
            .get(&symbol)
            .copied()
            .ok_or_else(|| {
                anyhow!(
                    "Lighter market {} unavailable: absent, inactive or reduce_only",
                    symbol
                )
            })
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
            symbol: to_global_symbol(&stats.symbol),
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
            symbol: to_global_symbol(&trade.symbol),
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

        let mut market_id_to_symbol: HashMap<u64, String> = HashMap::new();
        for symbol in &symbols {
            let market_id = client.get_market_id(symbol).await?;
            market_id_to_symbol.insert(market_id, to_global_symbol(&to_exchange_symbol(symbol)));
            let channel = format!("order_book/{}", market_id);
            self.subscribe(&mut ws_stream, channel).await?;
        }

        let stream = async_stream::stream! {
            use tokio::time::{interval, Duration};
            let mut keepalive = interval(Duration::from_secs(30));
            keepalive.tick().await;

            // Per-market BTreeMap state: snapshot on first message, delta after.
            let mut market_states: HashMap<u64, MarketState> = HashMap::new();

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
                                let ob_msg = match serde_json::from_str::<LighterWsOrderbook>(&text) {
                                    Ok(m) => m,
                                    Err(_) => continue,
                                };
                                let market_id: u64 = match ob_msg.channel
                                    .strip_prefix("order_book:")
                                    .and_then(|s| s.parse().ok())
                                {
                                    Some(id) => id,
                                    None => continue,
                                };
                                let symbol = match market_id_to_symbol.get(&market_id) {
                                    Some(s) => s.clone(),
                                    None => continue,
                                };

                                let result = if let Some(state) = market_states.get_mut(&market_id) {
                                    state.apply_delta(&ob_msg.order_book)
                                } else {
                                    let mut state = MarketState::new();
                                    let r = state.apply_snapshot(&ob_msg.order_book);
                                    if r.is_ok() { market_states.insert(market_id, state); }
                                    r
                                };
                                match result {
                                    Ok(()) => {
                                        if let Some(state) = market_states.get(&market_id) {
                                            yield Ok(state.to_orderbook(&symbol));
                                        }
                                    }
                                    Err(e) => {
                                        // Nonce gap — yield error and drop stale state.
                                        market_states.remove(&market_id);
                                        yield Err(anyhow!("Lighter orderbook nonce gap for {}: {}", symbol, e));
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
                                tracing::warn!("Lighter WebSocket stream ended");
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
            market_id_to_symbol.insert(market_id, to_global_symbol(&to_exchange_symbol(symbol)));

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
            keepalive.tick().await;

            // Per-market BTreeMap state for orderbook delta merging.
            let mut market_states: HashMap<u64, MarketState> = HashMap::new();

            loop {
                tokio::select! {
                    maybe_msg = ws_stream.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Text(text))) => {
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
                                    let market_id: u64 = match ob_msg.channel
                                        .strip_prefix("order_book:")
                                        .and_then(|s| s.parse().ok())
                                    {
                                        Some(id) => id,
                                        None => continue,
                                    };
                                    let symbol = match market_id_to_symbol.get(&market_id) {
                                        Some(s) => s.clone(),
                                        None => continue,
                                    };
                                    let result = if let Some(state) = market_states.get_mut(&market_id) {
                                        state.apply_delta(&ob_msg.order_book)
                                    } else {
                                        let mut state = MarketState::new();
                                        let r = state.apply_snapshot(&ob_msg.order_book);
                                        if r.is_ok() { market_states.insert(market_id, state); }
                                        r
                                    };
                                    match result {
                                        Ok(()) => {
                                            if let Some(state) = market_states.get(&market_id) {
                                                yield Ok(StreamEvent::Orderbook(state.to_orderbook(&symbol)));
                                            }
                                        }
                                        Err(e) => {
                                            market_states.remove(&market_id);
                                            yield Err(anyhow!("Lighter orderbook nonce gap for {}: {}", symbol, e));
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

#[cfg(test)]
mod tests {
    use super::*;
    fn update(begin: u64, nonce: u64, size: &str) -> LighterOrderBook {
        serde_json::from_value(serde_json::json!({"begin_nonce": begin, "nonce": nonce,
            "bids": [{"price": "100", "size": size}], "asks": [{"price": "101", "size": "1"}]}))
        .unwrap()
    }
    #[test]
    fn state_persists_per_market_and_a_gap_requires_a_new_snapshot() {
        let mut a = MarketState::new();
        let mut b = MarketState::new();
        a.apply_snapshot(&update(0, 10, "1")).unwrap();
        b.apply_snapshot(&update(0, 30, "7")).unwrap();
        a.apply_delta(&update(10, 11, "2")).unwrap();
        assert_eq!(a.to_orderbook("A").bids[0].quantity, Decimal::TWO);
        assert_eq!(b.to_orderbook("B").bids[0].quantity, Decimal::from(7));
        assert!(a.apply_delta(&update(12, 13, "3")).is_err());
        let mut replacement = MarketState::new();
        replacement.apply_snapshot(&update(0, 50, "9")).unwrap();
        assert_eq!(
            replacement.to_orderbook("A").bids[0].quantity,
            Decimal::from(9)
        );
    }
}
