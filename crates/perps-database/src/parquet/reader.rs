use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use arrow::compute::concat_batches;
use chrono::{DateTime, NaiveDate, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use perps_core::Orderbook;

use super::schema::{orderbook_arrow_schema, record_batch_to_orderbooks};

/// Reader for orderbook data stored in Parquet files.
///
/// Expects the following layout:
/// ```text
/// {base_dir}/{exchange}/{symbol}/YYYY-MM-DD.parquet
/// ```
pub struct OrderbookParquetReader {
    base_dir: PathBuf,
}

impl OrderbookParquetReader {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Read all orderbook snapshots for a given exchange/symbol/date.
    pub fn read_orderbooks(
        &self,
        exchange: &str,
        symbol: &str,
        date: NaiveDate,
    ) -> anyhow::Result<Vec<Orderbook>> {
        let path = self.file_path(exchange, symbol, date);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let batches = self.read_parquet_file(&path)?;
        if batches.is_empty() {
            return Ok(Vec::new());
        }

        // Merge all row groups into a single RecordBatch so that rows with the
        // same (ts, symbol) key are grouped together regardless of which row
        // group they originated from.
        let schema = Arc::new(orderbook_arrow_schema());
        let merged = concat_batches(&schema, batches.iter())
            .context("failed to concatenate Parquet row groups")?;

        Ok(record_batch_to_orderbooks(&merged, None, None))
    }

    /// Read the closest orderbook snapshot to a given timestamp.
    pub fn read_orderbook_at(
        &self,
        exchange: &str,
        symbol: &str,
        ts: DateTime<Utc>,
    ) -> anyhow::Result<Option<Orderbook>> {
        let date = ts.date_naive();
        let target_ts = ts.timestamp();
        let orderbooks = self.read_orderbooks(exchange, symbol, date)?;

        if orderbooks.is_empty() {
            return Ok(None);
        }

        // Find the orderbook with timestamp closest to target
        let closest = orderbooks
            .into_iter()
            .min_by_key(|ob| (ob.timestamp.timestamp() - target_ts).unsigned_abs());

        Ok(closest)
    }

    /// List available dates for a given exchange/symbol.
    pub fn list_dates(
        &self,
        exchange: &str,
        symbol: &str,
    ) -> anyhow::Result<Vec<NaiveDate>> {
        let dir = self.symbol_dir(exchange, symbol);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut dates = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory: {}", dir.display()))?
        {
            let entry = entry?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if let Some(date_str) = name.strip_suffix(".parquet") {
                if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                    dates.push(date);
                }
            }
        }

        dates.sort();
        Ok(dates)
    }

    /// List all available exchange/symbol pairs.
    pub fn list_pairs(&self) -> anyhow::Result<Vec<(String, String)>> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }

        let mut pairs = Vec::new();

        for exchange_entry in fs::read_dir(&self.base_dir)? {
            let exchange_entry = exchange_entry?;
            let exchange_path = exchange_entry.path();
            if !exchange_path.is_dir() {
                continue;
            }

            let exchange = exchange_entry.file_name().to_string_lossy().to_string();

            for symbol_entry in fs::read_dir(&exchange_path)? {
                let symbol_entry = symbol_entry?;
                if !symbol_entry.path().is_dir() {
                    continue;
                }

                let symbol = symbol_entry.file_name().to_string_lossy().to_string();
                pairs.push((exchange.clone(), symbol));
            }
        }

        pairs.sort();
        Ok(pairs)
    }

    /// Read all RecordBatches from a Parquet file.
    fn read_parquet_file(
        &self,
        path: &Path,
    ) -> anyhow::Result<Vec<arrow::record_batch::RecordBatch>> {
        let file = fs::File::open(path)
            .with_context(|| format!("failed to open Parquet file: {}", path.display()))?;

        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .context("failed to create Parquet reader")?
            .build()
            .context("failed to build Parquet batch reader")?;

        let mut batches = Vec::new();
        for batch_result in reader {
            let batch = batch_result.context("failed to read RecordBatch")?;
            batches.push(batch);
        }

        Ok(batches)
    }

    fn symbol_dir(&self, exchange: &str, symbol: &str) -> PathBuf {
        self.base_dir
            .join(exchange)
            .join(symbol)
    }

    fn file_path(&self, exchange: &str, symbol: &str, date: NaiveDate) -> PathBuf {
        self.symbol_dir(exchange, symbol)
            .join(format!("{}.parquet", date))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parquet::writer::OrderbookParquetWriter;
    use chrono::Utc;
    use perps_core::OrderbookLevel;
    use rust_decimal_macros::dec;
    use tempfile::TempDir;

    fn sample_orderbook(symbol: &str) -> Orderbook {
        Orderbook {
            symbol: symbol.to_string(),
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
    fn test_write_then_read() {
        let tmp_dir = TempDir::new().unwrap();

        // Write
        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer.flush_all().unwrap();

        // Read
        let reader = OrderbookParquetReader::new(tmp_dir.path());
        let today = Utc::now().date_naive();
        let orderbooks = reader.read_orderbooks("binance", "BTC", today).unwrap();

        assert_eq!(orderbooks.len(), 1);
        assert_eq!(orderbooks[0].symbol, "BTC");
        assert_eq!(orderbooks[0].bids.len(), 2);
        assert_eq!(orderbooks[0].asks.len(), 2);
    }

    #[test]
    fn test_read_nonexistent_returns_empty() {
        let tmp_dir = TempDir::new().unwrap();
        let reader = OrderbookParquetReader::new(tmp_dir.path());
        let today = Utc::now().date_naive();
        let orderbooks = reader.read_orderbooks("binance", "BTC", today).unwrap();
        assert!(orderbooks.is_empty());
    }

    #[test]
    fn test_list_dates() {
        let tmp_dir = TempDir::new().unwrap();

        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer.flush_all().unwrap();

        let reader = OrderbookParquetReader::new(tmp_dir.path());
        let dates = reader.list_dates("binance", "BTC").unwrap();
        assert_eq!(dates.len(), 1);
        assert_eq!(dates[0], Utc::now().date_naive());
    }

    #[test]
    fn test_list_pairs() {
        let tmp_dir = TempDir::new().unwrap();

        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer
            .write_orderbook("binance", "ETH", &sample_orderbook("ETH"))
            .unwrap();
        writer
            .write_orderbook("hyperliquid", "SOL", &sample_orderbook("SOL"))
            .unwrap();
        writer.flush_all().unwrap();

        let reader = OrderbookParquetReader::new(tmp_dir.path());
        let pairs = reader.list_pairs().unwrap();
        assert_eq!(pairs.len(), 3);
        assert!(pairs.contains(&("binance".to_string(), "BTC".to_string())));
        assert!(pairs.contains(&("binance".to_string(), "ETH".to_string())));
        assert!(pairs.contains(&("hyperliquid".to_string(), "SOL".to_string())));
    }

    #[test]
    fn test_read_orderbook_at() {
        let tmp_dir = TempDir::new().unwrap();

        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer.flush_all().unwrap();

        let reader = OrderbookParquetReader::new(tmp_dir.path());
        let result = reader.read_orderbook_at("binance", "BTC", Utc::now()).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().symbol, "BTC");
    }

    #[test]
    fn test_empty_orderbook() {
        let tmp_dir = TempDir::new().unwrap();

        let empty_ob = Orderbook {
            symbol: "BTC".to_string(),
            bids: vec![],
            asks: vec![],
            timestamp: Utc::now(),
        };

        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);
        writer.write_orderbook("binance", "BTC", &empty_ob).unwrap();
        // Empty orderbook produces 0 rows, so nothing is flushed
        writer.flush_all().unwrap();

        let reader = OrderbookParquetReader::new(tmp_dir.path());
        let today = Utc::now().date_naive();
        let orderbooks = reader.read_orderbooks("binance", "BTC", today).unwrap();
        assert!(orderbooks.is_empty());
    }
}
