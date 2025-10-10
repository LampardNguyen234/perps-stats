use anyhow::Result;
use perps_core::IPerps;
use perps_exchanges::BinanceClient;
use rust_decimal::Decimal;
use serde_json;

pub async fn execute(
    exchange: String,
    symbols: String,
    format: String,
    detailed: bool,
    timeframe: String,
) -> Result<()> {
    tracing::info!(
        "Retrieving L1 market data for {} from exchange {} (timeframe: {})",
        symbols,
        exchange,
        timeframe
    );

    // Parse symbols
    let symbol_list: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    // Create exchange client based on exchange name
    let client = match exchange.to_lowercase().as_str() {
        "binance" => BinanceClient::new(),
        _ => {
            anyhow::bail!("Unsupported exchange: {}. Currently supported: binance", exchange);
        }
    };

    // Fetch data for each symbol
    for symbol in &symbol_list {
        match fetch_and_display_market_data(&client, symbol, &format, detailed, &timeframe).await {
            Ok(_) => {}
            Err(e) => {
                tracing::error!("Failed to fetch data for {}: {}", symbol, e);
                eprintln!("Error fetching {}: {}", symbol, e);
            }
        }
    }

    Ok(())
}

async fn fetch_and_display_market_data(
    client: &BinanceClient,
    symbol: &str,
    format: &str,
    detailed: bool,
    timeframe: &str,
) -> Result<()> {
    // Fetch ticker data (L1) with timeframe support
    let ticker = client.get_ticker_with_timeframe(symbol, timeframe).await?;

    // Fetch funding rate
    let funding = client.get_funding_rate(symbol).await?;

    // Fetch open interest
    let open_interest = client.get_open_interest(symbol).await?;

    // Optionally fetch orderbook for detailed view
    let orderbook = if detailed {
        Some(client.get_orderbook(symbol, 10).await?)
    } else {
        None
    };

    match format.to_lowercase().as_str() {
        "json" => {
            display_json(&ticker, &funding, &open_interest, &orderbook, timeframe)?;
        }
        "table" => {
            display_table(&ticker, &funding, &open_interest, &orderbook, timeframe)?;
        }
        _ => {
            display_table(&ticker, &funding, &open_interest, &orderbook, timeframe)?;
        }
    }

    Ok(())
}

fn display_table(
    ticker: &perps_core::Ticker,
    funding: &perps_core::FundingRate,
    oi: &perps_core::OpenInterest,
    orderbook: &Option<perps_core::Orderbook>,
    timeframe: &str,
) -> Result<()> {
    println!("\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║  Market Data: {}                                    ", ticker.symbol);
    println!("╠══════════════════════════════════════════════════════════════════╣");

    // Price Information
    println!("║  PRICE INFORMATION");
    println!("║  ─────────────────────────────────────────────────────────────");
    println!("║  Last Price:        {:>20}", ticker.last_price);
    println!("║  Mark Price:        {:>20}", ticker.mark_price);
    println!("║  Index Price:       {:>20}", ticker.index_price);
    println!("║");

    // Best Bid/Ask (L1)
    println!("║  LEVEL 1 ORDERBOOK");
    println!("║  ─────────────────────────────────────────────────────────────");
    println!("║  Best Bid:          {:>20} @ {}", ticker.best_bid_price, ticker.best_bid_qty);
    println!("║  Best Ask:          {:>20} @ {}", ticker.best_ask_price, ticker.best_ask_qty);

    // Calculate mid-point price
    let mid_price = (ticker.best_bid_price + ticker.best_ask_price) / Decimal::new(2, 0);
    println!("║  Mid Price:         {:>20}", mid_price);

    let spread = ticker.best_ask_price - ticker.best_bid_price;
    let spread_pct = if ticker.best_bid_price > Decimal::ZERO {
        (spread / ticker.best_bid_price) * Decimal::new(100, 0)
    } else {
        Decimal::ZERO
    };
    println!("║  Spread:            {:>20} ({:.4}%)", spread, spread_pct);
    println!("║");

    // Statistics for the given timeframe
    let timeframe_upper = timeframe.to_uppercase();
    println!("║  {} STATISTICS", timeframe_upper);
    println!("║  ─────────────────────────────────────────────────────────────");
    println!("║  Volume:            {:>20}", ticker.volume_24h);
    println!("║  Turnover:          {:>20}", ticker.turnover_24h);
    println!("║  Price Change:      {:>20} ({:.2}%)",
        ticker.price_change_24h,
        ticker.price_change_pct * Decimal::new(100, 0)
    );
    println!("║  High:              {:>20}", ticker.high_price_24h);
    println!("║  Low:               {:>20}", ticker.low_price_24h);
    println!("║");

    // Funding Rate
    println!("║  FUNDING RATE");
    println!("║  ─────────────────────────────────────────────────────────────");
    println!("║  Current Rate:      {:>20} ({:.4}%)",
        funding.funding_rate,
        funding.funding_rate * Decimal::new(100, 0)
    );
    println!("║  Next Funding:      {}", funding.next_funding_time.format("%Y-%m-%d %H:%M:%S UTC"));
    println!("║  Interval:          {} hours", funding.funding_interval);
    println!("║");

    // Open Interest
    println!("║  OPEN INTEREST");
    println!("║  ─────────────────────────────────────────────────────────────");
    println!("║  Open Interest:     {:>20}", oi.open_interest);
    println!("║  Open Value:        {:>20}", oi.open_value);
    println!("║");

    // Detailed orderbook if requested
    if let Some(book) = orderbook {
        println!("║  ORDERBOOK DEPTH (Top 10)");
        println!("║  ─────────────────────────────────────────────────────────────");
        println!("║       Price          |      Bid Qty      |      Ask Qty     ");
        println!("║  ─────────────────────────────────────────────────────────────");

        let max_len = book.bids.len().max(book.asks.len()).min(10);
        for i in 0..max_len {
            let bid_str = if i < book.bids.len() {
                format!("{:>20} | {:>17}", book.bids[i].price, book.bids[i].quantity)
            } else {
                format!("{:>20} | {:>17}", "-", "-")
            };

            let ask_str = if i < book.asks.len() {
                format!("{:>20} | {:>17}", book.asks[i].price, book.asks[i].quantity)
            } else {
                format!("{:>20} | {:>17}", "-", "-")
            };

            println!("║  {} | {}", bid_str, ask_str);
        }
        println!("║");
    }

    println!("║  Timestamp: {}", ticker.timestamp.format("%Y-%m-%d %H:%M:%S UTC"));
    println!("╚══════════════════════════════════════════════════════════════════╝\n");

    Ok(())
}

fn display_json(
    ticker: &perps_core::Ticker,
    funding: &perps_core::FundingRate,
    oi: &perps_core::OpenInterest,
    orderbook: &Option<perps_core::Orderbook>,
    timeframe: &str,
) -> Result<()> {
    let stats_key = format!("statistics_{}", timeframe);
    let mut data = serde_json::json!({
        "symbol": ticker.symbol,
        "timeframe": timeframe,
        "price": {
            "last": ticker.last_price,
            "mark": ticker.mark_price,
            "index": ticker.index_price,
            "best_bid": ticker.best_bid_price,
            "best_bid_qty": ticker.best_bid_qty,
            "best_ask": ticker.best_ask_price,
            "best_ask_qty": ticker.best_ask_qty,
        },
    });

    data[stats_key] = serde_json::json!({
        "volume": ticker.volume_24h,
        "turnover": ticker.turnover_24h,
        "price_change": ticker.price_change_24h,
        "price_change_pct": ticker.price_change_pct,
        "high": ticker.high_price_24h,
        "low": ticker.low_price_24h,
    });

    data["funding_rate"] = serde_json::json!({
        "current": funding.funding_rate,
        "predicted": funding.predicted_rate,
        "next_funding_time": funding.next_funding_time,
        "interval_hours": funding.funding_interval,
    });

    data["open_interest"] = serde_json::json!({
        "quantity": oi.open_interest,
        "value": oi.open_value,
    });

    data["timestamp"] = serde_json::json!(ticker.timestamp);

    if let Some(book) = orderbook {
        data["orderbook"] = serde_json::json!({
            "bids": book.bids.iter().take(10).map(|level| {
                serde_json::json!({
                    "price": level.price,
                    "quantity": level.quantity,
                })
            }).collect::<Vec<_>>(),
            "asks": book.asks.iter().take(10).map(|level| {
                serde_json::json!({
                    "price": level.price,
                    "quantity": level.quantity,
                })
            }).collect::<Vec<_>>(),
            "timestamp": book.timestamp,
        });
    }

    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(())
}
