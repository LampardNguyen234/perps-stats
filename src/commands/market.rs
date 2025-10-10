use anyhow::Result;
use perps_core::IPerps;
use perps_exchanges::get_exchange;
use prettytable::{format, Cell, Row, Table};
use rust_decimal::Decimal;
use serde_json;

/// Holds all market data for a single symbol
struct MarketData {
    ticker: perps_core::Ticker,
    funding: perps_core::FundingRate,
    open_interest: perps_core::OpenInterest,
    orderbook: Option<perps_core::Orderbook>,
}

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

    // Create exchange client using factory
    let client = get_exchange(&exchange)?;

    // Parse symbols
    let requested_symbols: Vec<String> = symbols
        .split(',')
        .map(|s| client.parse_symbol(s.trim()))
        .collect();

    // Validate symbols - filter out unsupported ones
    let symbol_list = validate_symbols(client.as_ref(), &requested_symbols).await?;

    if symbol_list.is_empty() {
        anyhow::bail!("No valid symbols found for exchange {}", exchange);
    }

    // Fetch data for all symbols
    let mut market_data_list = Vec::new();
    for symbol in &symbol_list {
        match fetch_market_data(client.as_ref(), symbol, detailed, &timeframe).await {
            Ok(data) => market_data_list.push(data),
            Err(e) => {
                tracing::error!("Failed to fetch data for {}: {}", symbol, e);
                eprintln!("Error fetching {}: {}", symbol, e);
            }
        }
    }

    // Display based on format
    match format.to_lowercase().as_str() {
        "json" => display_all_json(&market_data_list, &timeframe)?,
        "table" => {
            for data in &market_data_list {
                display_table(&data.ticker, &data.funding, &data.open_interest, &data.orderbook, &timeframe)?;
            }
        }
        _ => {
            for data in &market_data_list {
                display_table(&data.ticker, &data.funding, &data.open_interest, &data.orderbook, &timeframe)?;
            }
        }
    }

    Ok(())
}

/// Fetch all market data for a single symbol
async fn fetch_market_data(
    client: &dyn IPerps,
    symbol: &str,
    detailed: bool,
    _timeframe: &str,
) -> Result<MarketData> {
    // Fetch ticker data (L1)
    // Note: Timeframe support is Binance-specific, using regular ticker for all exchanges
    let ticker = client.get_ticker(symbol).await?;

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

    Ok(MarketData {
        ticker,
        funding,
        open_interest,
        orderbook,
    })
}

fn display_table(
    ticker: &perps_core::Ticker,
    funding: &perps_core::FundingRate,
    oi: &perps_core::OpenInterest,
    orderbook: &Option<perps_core::Orderbook>,
    timeframe: &str,
) -> Result<()> {
    // Main table for market data
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_CLEAN);

    table.set_titles(Row::new(vec![Cell::new(&format!(
        "Market Data: {} ({})",
        ticker.symbol,
        timeframe.to_uppercase()
    ))
    .with_hspan(2)]));

    // Price Information
    table.add_row(Row::new(vec![
        Cell::new("Price Information").with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Last Price"),
        Cell::new_align(&ticker.last_price.to_string(), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Mark Price"),
        Cell::new_align(&ticker.mark_price.to_string(), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Index Price"),
        Cell::new_align(&ticker.index_price.to_string(), format::Alignment::RIGHT),
    ]));

    // L1 Orderbook
    table.add_row(Row::new(vec![
        Cell::new("Level 1 Orderbook").with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Best Bid"),
        Cell::new_align(
            &format!("{} @ {}", ticker.best_bid_price, ticker.best_bid_qty),
            format::Alignment::RIGHT,
        ),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Best Ask"),
        Cell::new_align(
            &format!("{} @ {}", ticker.best_ask_price, ticker.best_ask_qty),
            format::Alignment::RIGHT,
        ),
    ]));
    let mid_price = (ticker.best_bid_price + ticker.best_ask_price) / Decimal::new(2, 0);
    table.add_row(Row::new(vec![
        Cell::new("  Mid Price"),
        Cell::new_align(&mid_price.to_string(), format::Alignment::RIGHT),
    ]));
    let spread = ticker.best_ask_price - ticker.best_bid_price;
    let spread_pct = if ticker.best_bid_price > Decimal::ZERO {
        (spread / ticker.best_bid_price) * Decimal::new(100, 0)
    } else {
        Decimal::ZERO
    };
    table.add_row(Row::new(vec![
        Cell::new("  Spread"),
        Cell::new_align(
            &format!("{} ({:.4}%)", spread, spread_pct),
            format::Alignment::RIGHT,
        ),
    ]));

    // Timeframe Statistics
    table.add_row(Row::new(vec![
        Cell::new(&format!("{} Statistics", timeframe.to_uppercase()))
            .with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Volume"),
        Cell::new_align(&ticker.volume_24h.to_string(), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Turnover"),
        Cell::new_align(&ticker.turnover_24h.to_string(), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Price Change"),
        Cell::new_align(
            &format!(
                "{} ({:.2}%)",
                ticker.price_change_24h,
                ticker.price_change_pct * Decimal::new(100, 0)
            ),
            format::Alignment::RIGHT,
        ),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  High"),
        Cell::new_align(&ticker.high_price_24h.to_string(), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Low"),
        Cell::new_align(&ticker.low_price_24h.to_string(), format::Alignment::RIGHT),
    ]));

    // Funding Rate
    table.add_row(Row::new(vec![
        Cell::new("Funding Rate").with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Current Rate"),
        Cell::new_align(
            &format!(
                "{} ({:.4}%)",
                funding.funding_rate,
                funding.funding_rate * Decimal::new(100, 0)
            ),
            format::Alignment::RIGHT,
        ),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Next Funding"),
        Cell::new_align(
            &funding.next_funding_time.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            format::Alignment::RIGHT,
        ),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Interval"),
        Cell::new_align(
            &format!("{} hours", funding.funding_interval),
            format::Alignment::RIGHT,
        ),
    ]));

    // Open Interest
    table.add_row(Row::new(vec![
        Cell::new("Open Interest").with_style(prettytable::Attr::Bold),
        Cell::new(""),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Open Interest"),
        Cell::new_align(&oi.open_interest.to_string(), format::Alignment::RIGHT),
    ]));
    table.add_row(Row::new(vec![
        Cell::new("  Open Value"),
        Cell::new_align(&oi.open_value.to_string(), format::Alignment::RIGHT),
    ]));

    // Timestamp
    table.add_row(Row::new(vec![
        Cell::new("Timestamp").with_style(prettytable::Attr::Bold),
        Cell::new_align(
            &ticker.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            format::Alignment::RIGHT,
        ),
    ]));

    table.printstd();

    // Orderbook table if detailed
    if let Some(book) = orderbook {
        let mut book_table = Table::new();
        book_table.set_format(*format::consts::FORMAT_CLEAN);
        book_table.set_titles(Row::new(vec![Cell::new("Orderbook Depth (Top 10)").with_hspan(4)]));
        book_table.add_row(Row::new(vec![
            Cell::new("Bid Price").with_style(prettytable::Attr::Bold),
            Cell::new("Bid Qty").with_style(prettytable::Attr::Bold),
            Cell::new("Ask Price").with_style(prettytable::Attr::Bold),
            Cell::new("Ask Qty").with_style(prettytable::Attr::Bold),
        ]));

        let max_len = book.bids.len().max(book.asks.len()).min(10);
        for i in 0..max_len {
            let bid_price = book.bids.get(i).map_or("-".to_string(), |l| l.price.to_string());
            let bid_qty = book.bids.get(i).map_or("-".to_string(), |l| l.quantity.to_string());
            let ask_price = book.asks.get(i).map_or("-".to_string(), |l| l.price.to_string());
            let ask_qty = book.asks.get(i).map_or("-".to_string(), |l| l.quantity.to_string());

            book_table.add_row(Row::new(vec![
                Cell::new_align(&bid_price, format::Alignment::RIGHT),
                Cell::new_align(&bid_qty, format::Alignment::RIGHT),
                Cell::new_align(&ask_price, format::Alignment::RIGHT),
                Cell::new_align(&ask_qty, format::Alignment::RIGHT),
            ]));
        }
        println!();
        book_table.printstd();
    }

    Ok(())
}

/// Display all market data as a single JSON array
fn display_all_json(market_data_list: &[MarketData], timeframe: &str) -> Result<()> {
    let json_array: Vec<serde_json::Value> = market_data_list
        .iter()
        .map(|data| build_json_object(&data.ticker, &data.funding, &data.open_interest, &data.orderbook, timeframe))
        .collect();

    println!("{}", serde_json::to_string_pretty(&json_array)?);
    Ok(())
}

/// Build a JSON object for a single symbol's market data
fn build_json_object(
    ticker: &perps_core::Ticker,
    funding: &perps_core::FundingRate,
    oi: &perps_core::OpenInterest,
    orderbook: &Option<perps_core::Orderbook>,
    timeframe: &str,
) -> serde_json::Value {
    let stats_key = format!("statistics_{}", timeframe);

    // Calculate mid-point price
    let mid_price = (ticker.best_bid_price + ticker.best_ask_price) / Decimal::new(2, 0);

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
            "mid": mid_price,
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

    data
}

/// Validate symbols against the exchange - filter out unsupported symbols
async fn validate_symbols(client: &dyn IPerps, symbols: &[String]) -> Result<Vec<String>> {
    let mut valid_symbols = Vec::new();
    let mut invalid_symbols = Vec::new();

    for symbol in symbols {
        match client.is_supported(symbol).await {
            Ok(true) => {
                tracing::debug!("✓ Symbol {} is supported on {}", symbol, client.get_name());
                valid_symbols.push(symbol.clone());
            }
            Ok(false) => {
                tracing::warn!("✗ Symbol {} is not supported on {} - skipping", symbol, client.get_name());
                invalid_symbols.push(symbol.clone());
            }
            Err(e) => {
                tracing::error!("Failed to check if symbol {} is supported: {} - skipping", symbol, e);
                invalid_symbols.push(symbol.clone());
            }
        }
    }

    // Log summary
    if !invalid_symbols.is_empty() {
        eprintln!(
            "Warning: {} symbol(s) not supported on {}: {}",
            invalid_symbols.len(),
            client.get_name(),
            invalid_symbols.join(", ")
        );
    }

    if !valid_symbols.is_empty() {
        tracing::info!(
            "Validated {} symbol(s) on {}: {}",
            valid_symbols.len(),
            client.get_name(),
            valid_symbols.join(", ")
        );
    }

    Ok(valid_symbols)
}
