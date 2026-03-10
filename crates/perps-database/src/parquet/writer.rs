use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::NaiveDate;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use perps_core::Orderbook;

use super::schema::{orderbook_arrow_schema, orderbook_to_rows, rows_to_record_batch, OrderbookRow};

/// Key for grouping rows by exchange/symbol/date (maps to one Parquet file).
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct FileKey {
    exchange: String,
    symbol: String,
    date: NaiveDate,
}

/// Buffered Parquet writer for orderbook data.
///
/// Accumulates rows in memory and flushes to Parquet files when the buffer
/// exceeds `flush_threshold` rows. Files are written atomically (write to `.tmp`,
/// rename on success) to prevent corruption.
///
/// File layout:
/// ```text
/// {base_dir}/{exchange}/{symbol}/YYYY-MM-DD.parquet
/// ```
pub struct OrderbookParquetWriter {
    base_dir: PathBuf,
    buffers: HashMap<FileKey, Vec<OrderbookRow>>,
    flush_threshold: usize,
}

impl OrderbookParquetWriter {
    /// Create a new writer with the given base directory and flush threshold.
    pub fn new(base_dir: impl Into<PathBuf>, flush_threshold: usize) -> Self {
        Self {
            base_dir: base_dir.into(),
            buffers: HashMap::new(),
            flush_threshold,
        }
    }

    /// Create a new writer using default flush threshold (10,000 rows).
    pub fn with_defaults(base_dir: impl Into<PathBuf>) -> Self {
        Self::new(base_dir, 10_000)
    }

    /// Write an orderbook snapshot to the buffer. Flushes automatically when
    /// the buffer for the corresponding file exceeds `flush_threshold`.
    ///
    /// `symbol` is the global/normalized symbol (e.g., "BTC") used for the
    /// file path. This may differ from `orderbook.symbol` which can contain
    /// exchange-specific formats (e.g., "BTCUSDT", "BTC-USD-PERP").
    pub fn write_orderbook(&mut self, exchange: &str, symbol: &str, orderbook: &Orderbook) -> anyhow::Result<()> {
        let rows = orderbook_to_rows(exchange, orderbook);
        if rows.is_empty() {
            return Ok(());
        }

        let date = orderbook.timestamp.date_naive();

        let key = FileKey {
            exchange: exchange.to_string(),
            symbol: symbol.to_string(),
            date,
        };

        let buffer = self.buffers.entry(key.clone()).or_default();
        buffer.extend(rows);

        if buffer.len() >= self.flush_threshold {
            self.flush_key(&key)?;
        }

        Ok(())
    }

    /// Flush all buffered data to disk. Call this on shutdown or date rollover.
    pub fn flush_all(&mut self) -> anyhow::Result<()> {
        let keys: Vec<FileKey> = self.buffers.keys().cloned().collect();
        for key in keys {
            self.flush_key(&key)?;
        }
        Ok(())
    }

    /// Flush the buffer for a single file key to a Parquet file.
    fn flush_key(&mut self, key: &FileKey) -> anyhow::Result<()> {
        let rows = match self.buffers.remove(key) {
            Some(r) if !r.is_empty() => r,
            _ => return Ok(()),
        };

        let batch = rows_to_record_batch(&rows)
            .context("failed to build RecordBatch from orderbook rows")?;

        let dir = self.file_dir(key);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create directory: {}", dir.display()))?;

        let final_path = self.file_path(key);
        let tmp_path = final_path.with_extension("parquet.tmp");

        // If the file already exists, read existing data and merge
        let batches_to_write = if final_path.exists() {
            let existing = self.read_existing_file(&final_path)?;
            let mut all = existing;
            all.push(batch);
            all
        } else {
            vec![batch]
        };

        // Write to tmp file
        let schema = std::sync::Arc::new(orderbook_arrow_schema());
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_max_row_group_size(8192)
            .build();

        let file = fs::File::create(&tmp_path)
            .with_context(|| format!("failed to create tmp file: {}", tmp_path.display()))?;

        let mut writer = ArrowWriter::try_new(file, schema, Some(props))
            .context("failed to create ArrowWriter")?;

        for b in &batches_to_write {
            writer
                .write(b)
                .context("failed to write RecordBatch to Parquet")?;
        }

        writer.close().context("failed to close ArrowWriter")?;

        // Atomic rename
        fs::rename(&tmp_path, &final_path).with_context(|| {
            format!(
                "failed to rename {} -> {}",
                tmp_path.display(),
                final_path.display()
            )
        })?;

        tracing::debug!(
            "Flushed {} rows to {}",
            batches_to_write.iter().map(|b| b.num_rows()).sum::<usize>(),
            final_path.display()
        );

        Ok(())
    }

    /// Read all RecordBatches from an existing Parquet file.
    fn read_existing_file(&self, path: &Path) -> anyhow::Result<Vec<arrow::record_batch::RecordBatch>> {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

        let file = fs::File::open(path)
            .with_context(|| format!("failed to open existing file: {}", path.display()))?;

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

    /// Directory for a given file key.
    fn file_dir(&self, key: &FileKey) -> PathBuf {
        self.base_dir
            .join(&key.exchange)
            .join(&key.symbol)
    }

    /// Full file path for a given file key.
    fn file_path(&self, key: &FileKey) -> PathBuf {
        self.file_dir(key).join(format!("{}.parquet", key.date))
    }

    /// Returns the base directory for this writer.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_write_and_flush() {
        let tmp_dir = TempDir::new().unwrap();
        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);

        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer.flush_all().unwrap();

        let today = Utc::now().date_naive();
        let path = tmp_dir
            .path()
            .join("binance/BTC")
            .join(format!("{}.parquet", today));
        assert!(path.exists(), "Parquet file should exist at {}", path.display());
    }

    #[test]
    fn test_auto_flush_on_threshold() {
        let tmp_dir = TempDir::new().unwrap();
        // Flush after 5 rows (each orderbook has 4 levels = 4 rows)
        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 5);

        // First write: 4 rows, below threshold
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();

        let today = Utc::now().date_naive();
        let path = tmp_dir
            .path()
            .join("binance/BTC")
            .join(format!("{}.parquet", today));
        assert!(!path.exists(), "Should not flush yet (4 < 5)");

        // Second write: 4 more rows → 8 total, exceeds threshold → flush
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        assert!(path.exists(), "Should auto-flush after exceeding threshold");
    }

    #[test]
    fn test_append_to_existing_file() {
        let tmp_dir = TempDir::new().unwrap();
        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);

        // Write and flush once
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer.flush_all().unwrap();

        // Write and flush again (should append to existing file)
        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer.flush_all().unwrap();

        // Verify file still exists (atomic rename succeeded)
        let today = Utc::now().date_naive();
        let path = tmp_dir
            .path()
            .join("binance/BTC")
            .join(format!("{}.parquet", today));
        assert!(path.exists());
    }

    #[test]
    fn test_multiple_exchanges_and_symbols() {
        let tmp_dir = TempDir::new().unwrap();
        let mut writer = OrderbookParquetWriter::new(tmp_dir.path(), 100);

        writer
            .write_orderbook("binance", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer
            .write_orderbook("binance", "ETH", &sample_orderbook("ETH"))
            .unwrap();
        writer
            .write_orderbook("hyperliquid", "BTC", &sample_orderbook("BTC"))
            .unwrap();
        writer.flush_all().unwrap();

        let today = Utc::now().date_naive();
        let base = tmp_dir.path();

        assert!(base
            .join(format!("binance/BTC/{}.parquet", today))
            .exists());
        assert!(base
            .join(format!("binance/ETH/{}.parquet", today))
            .exists());
        assert!(base
            .join(format!("hyperliquid/BTC/{}.parquet", today))
            .exists());
    }
}
