use anyhow::Result;
use clap::Args;
use futures::StreamExt;
use perps_core::streaming::*;
use perps_core::types::Kline;
use perps_core::IPerps;
use perps_database::{PostgresRepository, Repository};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct StreamArgs {
    /// Exchange to stream from (supported: aster, binance, hyperliquid, bybit, kucoin, lighter, paradex)
    #[arg(short, long, default_value = "binance")]
    pub exchange: String,

    /// Symbols to stream (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub symbols: Vec<String>,

    /// Data types to stream (comma-separated: ticker,trade,orderbook,fundingrate,kline)
    #[arg(short, long, value_delimiter = ',', default_value = "ticker,trade")]
    pub data_types: Vec<String>,

    /// Klines interval/timeframe (e.g., 1m, 5m, 15m, 1h, 4h, 1d). Required when streaming klines.
    #[arg(long)]
    pub klines_interval: Option<String>,

    /// Database URL for storing streamed data
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    /// Batch size for database writes (number of events before writing)
    #[arg(long, default_value = "100")]
    pub batch_size: usize,

    /// Maximum time to run (in seconds, 0 for infinite)
    #[arg(long, default_value = "0")]
    pub max_duration: u64,
}

pub async fn execute(args: StreamArgs) -> Result<()> {
    tracing::info!(
        "Starting real-time streaming for exchange: {}",
        args.exchange
    );
    tracing::info!("Symbols: {:?}", args.symbols);
    tracing::info!("Data types: {:?}", args.data_types);

    // Validate exchange
    if !matches!(
        args.exchange.as_str(),
        "aster" | "binance" | "hyperliquid" | "bybit" | "kucoin" | "lighter" | "paradex"
    ) {
        anyhow::bail!("Only 'aster', 'binance', 'hyperliquid', 'bybit', 'kucoin', 'lighter', and 'paradex' exchanges are currently supported for streaming");
    }

    // Parse data types
    let mut data_types = Vec::new();
    for dt in &args.data_types {
        match dt.to_lowercase().as_str() {
            "ticker" => data_types.push(StreamDataType::Ticker),
            "trade" => data_types.push(StreamDataType::Trade),
            "orderbook" => data_types.push(StreamDataType::Orderbook),
            "fundingrate" | "funding" => data_types.push(StreamDataType::FundingRate),
            "kline" | "klines" | "candle" | "candles" => data_types.push(StreamDataType::Kline),
            _ => {
                tracing::warn!("Unknown data type '{}', skipping", dt);
            }
        }
    }

    if data_types.is_empty() {
        anyhow::bail!("No valid data types specified");
    }

    // Validate klines interval if klines are requested
    if data_types.contains(&StreamDataType::Kline) && args.klines_interval.is_none() {
        anyhow::bail!(
            "--klines-interval is required when streaming klines. Example: --klines-interval 1h"
        );
    }

    // Validate data types for exchange
    if args.exchange == "hyperliquid"
        && data_types.contains(&StreamDataType::FundingRate)
        && data_types.len() == 1
    {
        anyhow::bail!("Hyperliquid does not support funding rate streaming via WebSocket. Please use other data types: ticker, trade, orderbook");
    }

    if args.exchange == "kucoin" && data_types.contains(&StreamDataType::Orderbook) {
        anyhow::bail!("KuCoin orderbook streaming is not yet implemented (requires incremental update handling). Please use ticker or trade data types");
    }

    if (args.exchange == "kucoin" || args.exchange == "bybit" || args.exchange == "lighter")
        && data_types.contains(&StreamDataType::FundingRate)
    {
        anyhow::bail!(
            "{} does not support funding rate streaming via WebSocket. Please use other data types",
            args.exchange
        );
    }

    // Validate klines support per exchange
    if data_types.contains(&StreamDataType::Kline) {
        match args.exchange.as_str() {
            "kucoin" => {
                // KuCoin supports klines via WebSocket
                tracing::info!(
                    "Streaming klines from KuCoin via WebSocket (interval: {})",
                    args.klines_interval.as_ref().unwrap()
                );
            }
            _ => {
                anyhow::bail!("Klines streaming is currently only supported for KuCoin exchange. Other exchanges use REST API for klines. Use the 'start' command for REST API klines fetching.");
            }
        }
    }

    // Note: Paradex supports funding rate streaming via WebSocket

    // Initialize database connection if URL provided
    let repository: Option<Arc<Mutex<PostgresRepository>>> =
        if let Some(db_url) = &args.database_url {
            tracing::info!("Connecting to database for storage");
            let pool = PgPool::connect(db_url).await?;
            Some(Arc::new(Mutex::new(PostgresRepository::new(pool))))
        } else {
            tracing::warn!("No database URL provided, data will not be persisted");
            None
        };

    // Convert symbols to exchange format based on the exchange
    let parsed_symbols: Vec<String> = match args.exchange.as_str() {
        "aster" => {
            // For Aster WebSocket, uses Binance-compatible format (BTCUSDT, no hyphens)
            let aster_client = perps_exchanges::aster::AsterClient::new_rest_only();
            args.symbols
                .iter()
                .map(|s| {
                    let normalized = aster_client.parse_symbol(s);
                    // Convert BTC-USDT to BTCUSDT (remove hyphens for WebSocket)
                    normalized.replace("-", "")
                })
                .collect()
        }
        "binance" => {
            // For Binance WebSocket, we need BTCUSDT format (no hyphens)
            let binance_client = perps_exchanges::binance::BinanceClient::new_rest_only();
            args.symbols
                .iter()
                .map(|s| {
                    let normalized = binance_client.parse_symbol(s);
                    // Convert BTC-USDT to BTCUSDT (remove hyphens for WebSocket)
                    normalized.replace("-", "")
                })
                .collect()
        }
        "hyperliquid" => {
            // For Hyperliquid, symbols are simple uppercase (BTC, ETH, etc.)
            args.symbols.iter().map(|s| s.to_uppercase()).collect()
        }
        "bybit" => {
            // For Bybit, use BTCUSDT format
            let bybit_client = perps_exchanges::bybit::BybitClient::new();
            args.symbols
                .iter()
                .map(|s| bybit_client.parse_symbol(s))
                .collect()
        }
        "kucoin" => {
            // For KuCoin, use XBTUSDTM format
            let kucoin_client = perps_exchanges::kucoin::KucoinClient::new_rest_only();
            args.symbols
                .iter()
                .map(|s| kucoin_client.parse_symbol(s))
                .collect()
        }
        "lighter" => {
            // For Lighter, symbols are simple uppercase (BTC, ETH, etc.)
            let lighter_client = perps_exchanges::lighter::LighterClient::new();
            args.symbols
                .iter()
                .map(|s| lighter_client.parse_symbol(s))
                .collect()
        }
        "paradex" => {
            // For Paradex, use BTC-USD-PERP format
            let paradex_client = perps_exchanges::paradex::ParadexClient::new();
            args.symbols
                .iter()
                .map(|s| paradex_client.parse_symbol(s))
                .collect()
        }
        _ => args.symbols.clone(),
    };

    tracing::debug!("Converted symbols for WebSocket: {:?}", parsed_symbols);

    // Create stream configuration
    let config = StreamConfig {
        symbols: parsed_symbols.clone(),
        data_types: data_types.clone(),
        auto_reconnect: true,
        kline_interval: args.klines_interval.clone(),
    };

    // Start streaming based on exchange
    tracing::info!("Connecting to WebSocket stream...");
    let mut stream: Box<dyn futures::Stream<Item = Result<StreamEvent>> + Unpin + Send> =
        match args.exchange.as_str() {
            "aster" => {
                let ws_client = perps_exchanges::aster::AsterWsClient::new();
                Box::new(ws_client.stream_multi(config).await?)
            }
            "binance" => {
                let ws_client = perps_exchanges::binance::BinanceWsClient::new();
                Box::new(ws_client.stream_multi(config).await?)
            }
            "hyperliquid" => {
                let ws_client = perps_exchanges::hyperliquid::HyperliquidWsClient::new();
                Box::new(ws_client.stream_multi(config).await?)
            }
            "bybit" => {
                let ws_client = perps_exchanges::bybit::BybitWsClient::new();
                Box::new(ws_client.stream_multi(config).await?)
            }
            "kucoin" => {
                let ws_client = perps_exchanges::kucoin::KuCoinWsClient::new();
                Box::new(ws_client.stream_multi(config).await?)
            }
            "lighter" => {
                let ws_client = perps_exchanges::lighter::LighterWsClient::new();
                Box::new(ws_client.stream_multi(config).await?)
            }
            "paradex" => {
                let ws_client = perps_exchanges::paradex::ParadexWsClient::new();
                Box::new(ws_client.stream_multi(config).await?)
            }
            _ => anyhow::bail!("Unsupported exchange for streaming: {}", args.exchange),
        };

    // Batch buffers
    let mut ticker_buffer = Vec::new();
    let mut trade_buffer = Vec::new();
    let mut orderbook_buffer = Vec::new();
    let mut funding_rate_buffer = Vec::new();
    let mut kline_buffer: Vec<Kline> = Vec::new();

    let mut event_count = 0;
    let start_time = tokio::time::Instant::now();

    // Setup Ctrl+C handler
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                tracing::info!("Received Ctrl+C, stopping stream...");
                shutdown_clone.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Err(e) => {
                tracing::error!("Failed to listen for Ctrl+C signal: {}", e);
            }
        }
    });

    // Stream processing loop
    loop {
        // Use tokio::select! to handle both stream events and shutdown/timeout signals
        tokio::select! {
            maybe_event = stream.next() => {
                match maybe_event {
            Some(Ok(event)) => {
                event_count += 1;

                match event {
                    StreamEvent::Ticker(ticker) => {
                        tracing::debug!("Received ticker: {} @ {}", ticker.symbol, ticker.last_price);
                        ticker_buffer.push(ticker);

                        // Flush ticker buffer when batch size reached
                        if ticker_buffer.len() >= args.batch_size {
                            if let Some(repo) = &repository {
                                let repo = repo.lock().await;
                                if let Err(e) = repo.store_tickers_with_exchange(&args.exchange, &ticker_buffer).await {
                                    tracing::error!("Failed to store tickers: {}", e);
                                }
                            }
                            ticker_buffer.clear();
                        }
                    }
                    StreamEvent::Trade(trade) => {
                        tracing::debug!("Received trade: {} {} @ {}", trade.symbol, trade.quantity, trade.price);
                        trade_buffer.push(trade);

                        // Flush trade buffer when batch size reached
                        if trade_buffer.len() >= args.batch_size {
                            if let Some(repo) = &repository {
                                let repo = repo.lock().await;
                                if let Err(e) = repo.store_trades_with_exchange(&args.exchange, &trade_buffer).await {
                                    tracing::error!("Failed to store trades: {}", e);
                                }
                            }
                            trade_buffer.clear();
                        }
                    }
                    StreamEvent::Orderbook(orderbook) => {
                        tracing::debug!("Received orderbook: {} (bids: {}, asks: {})",
                            orderbook.symbol, orderbook.bids.len(), orderbook.asks.len());
                        orderbook_buffer.push(orderbook);

                        // Flush orderbook buffer when batch size reached
                        if orderbook_buffer.len() >= args.batch_size {
                            if let Some(repo) = &repository {
                                let repo = repo.lock().await;
                                if let Err(e) = repo.store_orderbooks_with_exchange(&args.exchange, &orderbook_buffer).await {
                                    tracing::error!("Failed to store orderbooks: {}", e);
                                }
                            }
                            orderbook_buffer.clear();
                        }
                    }
                    StreamEvent::FundingRate(funding_rate) => {
                        tracing::debug!("Received funding rate: {} @ {}", funding_rate.symbol, funding_rate.funding_rate);
                        funding_rate_buffer.push(funding_rate);

                        // Flush funding rate buffer when batch size reached
                        if funding_rate_buffer.len() >= args.batch_size {
                            if let Some(repo) = &repository {
                                let repo = repo.lock().await;
                                if let Err(e) = repo.store_funding_rates_with_exchange(&args.exchange, &funding_rate_buffer).await {
                                    tracing::error!("Failed to store funding rates: {}", e);
                                }
                            }
                            funding_rate_buffer.clear();
                        }
                    }
                    StreamEvent::Kline(kline) => {
                        tracing::debug!("Received kline: {} (interval: {}, open: {}, close: {})",
                            kline.symbol, kline.interval, kline.open, kline.close);
                        kline_buffer.push(kline);

                        // Flush kline buffer when batch size reached
                        if kline_buffer.len() >= args.batch_size {
                            if let Some(repo) = &repository {
                                let repo = repo.lock().await;
                                if let Err(e) = repo.store_klines_with_exchange(&args.exchange, &kline_buffer).await {
                                    tracing::error!("Failed to store klines: {}", e);
                                }
                            }
                            kline_buffer.clear();
                        }
                    }
                }

                // Log progress every 1000 events
                if event_count % 10 == 0 {
                    tracing::info!("Processed {} events", event_count);
                }
            }
            Some(Err(e)) => {
                tracing::error!("Stream error: {}", e);
                break;
            }
            None => {
                tracing::info!("Stream ended");
                break;
            }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                // Check shutdown signal or max duration periodically
                if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    tracing::info!("Shutdown signal received, flushing buffers...");
                    break;
                }

                // Check max duration
                if args.max_duration > 0 && start_time.elapsed().as_secs() >= args.max_duration {
                    tracing::info!("Reached maximum duration, stopping stream");
                    break;
                }
            }
        }
    }

    // Flush remaining buffers
    if let Some(repo) = &repository {
        let repo = repo.lock().await;

        if !ticker_buffer.is_empty() {
            if let Err(e) = repo
                .store_tickers_with_exchange(&args.exchange, &ticker_buffer)
                .await
            {
                tracing::error!("Failed to flush ticker buffer: {}", e);
            }
        }

        if !trade_buffer.is_empty() {
            if let Err(e) = repo
                .store_trades_with_exchange(&args.exchange, &trade_buffer)
                .await
            {
                tracing::error!("Failed to flush trade buffer: {}", e);
            }
        }

        if !orderbook_buffer.is_empty() {
            if let Err(e) = repo
                .store_orderbooks_with_exchange(&args.exchange, &orderbook_buffer)
                .await
            {
                tracing::error!("Failed to flush orderbook buffer: {}", e);
            }
        }

        if !funding_rate_buffer.is_empty() {
            if let Err(e) = repo
                .store_funding_rates_with_exchange(&args.exchange, &funding_rate_buffer)
                .await
            {
                tracing::error!("Failed to flush funding rate buffer: {}", e);
            }
        }

        if !kline_buffer.is_empty() {
            if let Err(e) = repo
                .store_klines_with_exchange(&args.exchange, &kline_buffer)
                .await
            {
                tracing::error!("Failed to flush kline buffer: {}", e);
            }
        }
    }

    tracing::info!(
        "Streaming completed. Total events processed: {}",
        event_count
    );
    Ok(())
}
