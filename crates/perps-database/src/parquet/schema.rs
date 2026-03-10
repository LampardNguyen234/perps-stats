use std::sync::Arc;

use arrow::array::{
    Float64Array, Int64Array, StringArray, StringBuilder, UInt16Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use perps_core::{Orderbook, OrderbookLevel};
use rust_decimal::prelude::ToPrimitive;

/// One row in the Parquet file: a single price level at one snapshot.
#[derive(Debug, Clone)]
pub struct OrderbookRow {
    pub ts: i64,
    pub exchange: String,
    pub symbol: String,
    pub side: String,
    pub level: u16,
    pub price: f64,
    pub quantity: f64,
}

/// Returns the Arrow schema used for orderbook Parquet files.
pub fn orderbook_arrow_schema() -> Schema {
    Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("exchange", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, false),
        Field::new("side", DataType::Utf8, false),
        Field::new("level", DataType::UInt16, false),
        Field::new("price", DataType::Float64, false),
        Field::new("quantity", DataType::Float64, false),
    ])
}

/// Convert an `Orderbook` into a vector of `OrderbookRow` (one row per level per side).
pub fn orderbook_to_rows(exchange: &str, orderbook: &Orderbook) -> Vec<OrderbookRow> {
    let ts = orderbook.timestamp.timestamp();
    let symbol = orderbook.symbol.clone();

    let mut rows = Vec::with_capacity(orderbook.bids.len() + orderbook.asks.len());

    for (i, level) in orderbook.bids.iter().enumerate() {
        rows.push(OrderbookRow {
            ts,
            exchange: exchange.to_string(),
            symbol: symbol.clone(),
            side: "bid".to_string(),
            level: i as u16,
            price: level.price.to_f64().unwrap_or(0.0),
            quantity: level.quantity.to_f64().unwrap_or(0.0),
        });
    }

    for (i, level) in orderbook.asks.iter().enumerate() {
        rows.push(OrderbookRow {
            ts,
            exchange: exchange.to_string(),
            symbol: symbol.clone(),
            side: "ask".to_string(),
            level: i as u16,
            price: level.price.to_f64().unwrap_or(0.0),
            quantity: level.quantity.to_f64().unwrap_or(0.0),
        });
    }

    rows
}

/// Build an Arrow `RecordBatch` from a slice of `OrderbookRow`.
pub fn rows_to_record_batch(rows: &[OrderbookRow]) -> anyhow::Result<RecordBatch> {
    let schema = Arc::new(orderbook_arrow_schema());

    let ts_array = Int64Array::from(rows.iter().map(|r| r.ts).collect::<Vec<_>>());

    let mut exchange_builder = StringBuilder::new();
    let mut symbol_builder = StringBuilder::new();
    let mut side_builder = StringBuilder::new();
    for row in rows {
        exchange_builder.append_value(&row.exchange);
        symbol_builder.append_value(&row.symbol);
        side_builder.append_value(&row.side);
    }

    let level_array = UInt16Array::from(rows.iter().map(|r| r.level).collect::<Vec<_>>());
    let price_array = Float64Array::from(rows.iter().map(|r| r.price).collect::<Vec<_>>());
    let quantity_array = Float64Array::from(rows.iter().map(|r| r.quantity).collect::<Vec<_>>());

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(ts_array),
            Arc::new(exchange_builder.finish()),
            Arc::new(symbol_builder.finish()),
            Arc::new(side_builder.finish()),
            Arc::new(level_array),
            Arc::new(price_array),
            Arc::new(quantity_array),
        ],
    )?;

    Ok(batch)
}

/// Reconstruct `Orderbook` structs from a `RecordBatch`.
///
/// Groups rows by `ts` → splits by `side` → sorts bids descending by price,
/// asks ascending by price → builds `Orderbook`.
pub fn record_batch_to_orderbooks(
    batch: &RecordBatch,
    exchange_filter: Option<&str>,
    symbol_filter: Option<&str>,
) -> Vec<Orderbook> {
    use chrono::{DateTime, TimeZone, Utc};
    use rust_decimal::Decimal;
    use std::collections::BTreeMap;
    use std::str::FromStr;

    let ts_col = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("ts column");
    let exchange_col = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("exchange column");
    let symbol_col = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("symbol column");
    let side_col = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("side column");
    let level_col = batch
        .column(4)
        .as_any()
        .downcast_ref::<UInt16Array>()
        .expect("level column");
    let price_col = batch
        .column(5)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("price column");
    let quantity_col = batch
        .column(6)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("quantity column");

    // Group by (ts, symbol) → (bids, asks)
    // Using BTreeMap for deterministic ordering by timestamp
    let mut groups: BTreeMap<(i64, String), (Vec<OrderbookLevel>, Vec<OrderbookLevel>)> =
        BTreeMap::new();

    for i in 0..batch.num_rows() {
        let exchange = exchange_col.value(i);
        let symbol = symbol_col.value(i);

        if let Some(ef) = exchange_filter {
            if exchange != ef {
                continue;
            }
        }
        if let Some(sf) = symbol_filter {
            if symbol != sf {
                continue;
            }
        }

        let ts = ts_col.value(i);
        let side = side_col.value(i);
        let _level = level_col.value(i);
        let price = price_col.value(i);
        let quantity = quantity_col.value(i);

        let level = OrderbookLevel {
            price: Decimal::from_str(&format!("{}", price)).unwrap_or(Decimal::ZERO),
            quantity: Decimal::from_str(&format!("{}", quantity)).unwrap_or(Decimal::ZERO),
        };

        let entry = groups
            .entry((ts, symbol.to_string()))
            .or_insert_with(|| (Vec::new(), Vec::new()));

        match side {
            "bid" => entry.0.push(level),
            "ask" => entry.1.push(level),
            _ => {}
        }
    }

    groups
        .into_iter()
        .map(|((ts, symbol), (mut bids, mut asks))| {
            // Sort bids descending by price, asks ascending by price
            bids.sort_by(|a, b| b.price.cmp(&a.price));
            asks.sort_by(|a, b| a.price.cmp(&b.price));

            let timestamp: DateTime<Utc> = Utc
                .timestamp_opt(ts, 0)
                .single()
                .unwrap_or_else(Utc::now);

            Orderbook {
                symbol,
                bids,
                asks,
                timestamp,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    fn sample_orderbook() -> Orderbook {
        Orderbook {
            symbol: "BTC".to_string(),
            bids: vec![
                OrderbookLevel {
                    price: dec!(100000),
                    quantity: dec!(1.5),
                },
                OrderbookLevel {
                    price: dec!(99900),
                    quantity: dec!(2.0),
                },
            ],
            asks: vec![
                OrderbookLevel {
                    price: dec!(100100),
                    quantity: dec!(1.0),
                },
                OrderbookLevel {
                    price: dec!(100200),
                    quantity: dec!(3.0),
                },
            ],
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_orderbook_to_rows() {
        let ob = sample_orderbook();
        let rows = orderbook_to_rows("binance", &ob);
        assert_eq!(rows.len(), 4); // 2 bids + 2 asks
        assert_eq!(rows[0].side, "bid");
        assert_eq!(rows[0].level, 0);
        assert_eq!(rows[2].side, "ask");
        assert_eq!(rows[2].level, 0);
    }

    #[test]
    fn test_rows_to_record_batch() {
        let ob = sample_orderbook();
        let rows = orderbook_to_rows("binance", &ob);
        let batch = rows_to_record_batch(&rows).expect("should build batch");
        assert_eq!(batch.num_rows(), 4);
        assert_eq!(batch.num_columns(), 7);
    }

    #[test]
    fn test_roundtrip() {
        let ob = sample_orderbook();
        let ts = ob.timestamp.timestamp();
        let rows = orderbook_to_rows("binance", &ob);
        let batch = rows_to_record_batch(&rows).expect("should build batch");
        let reconstructed = record_batch_to_orderbooks(&batch, None, None);

        assert_eq!(reconstructed.len(), 1);
        let r = &reconstructed[0];
        assert_eq!(r.symbol, "BTC");
        assert_eq!(r.bids.len(), 2);
        assert_eq!(r.asks.len(), 2);
        assert_eq!(r.timestamp.timestamp(), ts);
        // Bids should be sorted descending
        assert!(r.bids[0].price > r.bids[1].price);
        // Asks should be sorted ascending
        assert!(r.asks[0].price < r.asks[1].price);
    }
}
