use anyhow::Result;
use clap::Args;
use futures::StreamExt;
use perps_core::streaming::*;
use perps_core::IPerps;
use perps_database::{PostgresRepository, Repository};
use perps_exchanges::binance::BinanceWsClient;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct StreamArgs {
    /// Exchange to stream from (currently only 'binance' supported)
    #[arg(short, long, default_value = "binance")]
    pub exchange: String,

    /// Symbols to stream (comma-separated)
    #[arg(short, long, value_delimiter = ',')]
    pub symbols: Vec<String>,

    /// Data types to stream (comma-separated: ticker,trade,orderbook,fundingrate)
    #[arg(short, long, value_delimiter = ',', default_value = "ticker,trade")]
    pub data_types: Vec<String>,

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
    tracing::info!("Starting real-time streaming for exchange: {}", args.exchange);
    tracing::info!("Symbols: {:?}", args.symbols);
    tracing::info!("Data types: {:?}", args.data_types);

    // Validate exchange
    if args.exchange != "binance" {
        anyhow::bail!("Only 'binance' exchange is currently supported for streaming");
    }

    // Parse data types
    let mut data_types = Vec::new();
    for dt in &args.data_types {
        match dt.to_lowercase().as_str() {
            "ticker" => data_types.push(StreamDataType::Ticker),
            "trade" => data_types.push(StreamDataType::Trade),
            "orderbook" => data_types.push(StreamDataType::Orderbook),
            "fundingrate" | "funding" => data_types.push(StreamDataType::FundingRate),
            _ => {
                tracing::warn!("Unknown data type '{}', skipping", dt);
            }
        }
    }

    if data_types.is_empty() {
        anyhow::bail!("No valid data types specified");
    }

    // Initialize database connection if URL provided
    let repository: Option<Arc<Mutex<PostgresRepository>>> = if let Some(db_url) = &args.database_url {
        tracing::info!("Connecting to database for storage");
        let pool = PgPool::connect(db_url).await?;
        Some(Arc::new(Mutex::new(PostgresRepository::new(pool))))
    } else {
        tracing::warn!("No database URL provided, data will not be persisted");
        None
    };

    // Convert symbols to exchange format
    // For Binance WebSocket, we need BTCUSDT format (no hyphens)
    let binance_client = perps_exchanges::binance::BinanceClient::new();
    let parsed_symbols: Vec<String> = args.symbols
        .iter()
        .map(|s| {
            let normalized = binance_client.parse_symbol(s);
            // Convert BTC-USDT to BTCUSDT (remove hyphens for WebSocket)
            normalized.replace("-", "")
        })
        .collect();

    tracing::debug!("Converted symbols for WebSocket: {:?}", parsed_symbols);

    // Create WebSocket client
    let ws_client = BinanceWsClient::new();

    // Create stream configuration
    let config = StreamConfig {
        symbols: parsed_symbols.clone(),
        data_types: data_types.clone(),
        auto_reconnect: true,
    };

    // Start streaming
    tracing::info!("Connecting to WebSocket stream...");
    let mut stream = ws_client.stream_multi(config).await?;

    // Batch buffers
    let mut ticker_buffer = Vec::new();
    let mut trade_buffer = Vec::new();
    let mut orderbook_buffer = Vec::new();
    let mut funding_rate_buffer = Vec::new();

    let mut event_count = 0;
    let start_time = tokio::time::Instant::now();

    // Stream processing loop
    loop {
        // Check max duration
        if args.max_duration > 0 && start_time.elapsed().as_secs() >= args.max_duration {
            tracing::info!("Reached maximum duration, stopping stream");
            break;
        }

        match stream.next().await {
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

    // Flush remaining buffers
    if let Some(repo) = &repository {
        let repo = repo.lock().await;

        if !ticker_buffer.is_empty() {
            if let Err(e) = repo.store_tickers_with_exchange(&args.exchange, &ticker_buffer).await {
                tracing::error!("Failed to flush ticker buffer: {}", e);
            }
        }

        if !trade_buffer.is_empty() {
            if let Err(e) = repo.store_trades_with_exchange(&args.exchange, &trade_buffer).await {
                tracing::error!("Failed to flush trade buffer: {}", e);
            }
        }

        if !orderbook_buffer.is_empty() {
            if let Err(e) = repo.store_orderbooks_with_exchange(&args.exchange, &orderbook_buffer).await {
                tracing::error!("Failed to flush orderbook buffer: {}", e);
            }
        }

        if !funding_rate_buffer.is_empty() {
            if let Err(e) = repo.store_funding_rates_with_exchange(&args.exchange, &funding_rate_buffer).await {
                tracing::error!("Failed to flush funding rate buffer: {}", e);
            }
        }
    }

    tracing::info!("Streaming completed. Total events processed: {}", event_count);
    Ok(())
}
