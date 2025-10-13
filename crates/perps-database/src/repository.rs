use async_trait::async_trait;
use perps_core::*;
use rust_decimal::prelude::ToPrimitive;
use sqlx::{types::chrono::{DateTime, Utc, NaiveDate}, PgPool, Row};

/// Repository trait defines database operations for perps data.
/// This abstraction allows for easier testing and potential migration to other databases.
#[async_trait]
pub trait Repository: Send + Sync {
    /// Store market information
    async fn store_markets(&self, markets: &[Market]) -> anyhow::Result<()>;

    /// Store ticker data
    async fn store_tickers(&self, tickers: &[Ticker]) -> anyhow::Result<()>;

    /// Store ticker data with exchange information
    async fn store_tickers_with_exchange(&self, exchange: &str, tickers: &[Ticker]) -> anyhow::Result<()>;

    /// Store orderbook snapshots
    async fn store_orderbooks(&self, orderbooks: &[Orderbook]) -> anyhow::Result<()>;

    /// Store orderbook snapshots with exchange information
    async fn store_orderbooks_with_exchange(&self, exchange: &str, orderbooks: &[Orderbook]) -> anyhow::Result<()>;

    /// Store funding rates
    async fn store_funding_rates(&self, rates: &[FundingRate]) -> anyhow::Result<()>;

    /// Store funding rates with exchange information
    async fn store_funding_rates_with_exchange(&self, exchange: &str, rates: &[FundingRate]) -> anyhow::Result<()>;

    /// Store open interest data
    async fn store_open_interest(&self, oi: &[OpenInterest]) -> anyhow::Result<()>;

    /// Store open interest data with exchange information
    async fn store_open_interest_with_exchange(&self, exchange: &str, oi: &[OpenInterest]) -> anyhow::Result<()>;

    /// Store klines (OHLCV data)
    async fn store_klines(&self, klines: &[Kline]) -> anyhow::Result<()>;

    /// Store klines with exchange information
    async fn store_klines_with_exchange(&self, exchange: &str, klines: &[Kline]) -> anyhow::Result<()>;

    /// Store trades
    async fn store_trades(&self, trades: &[Trade]) -> anyhow::Result<()>;

    /// Store trades with exchange information
    async fn store_trades_with_exchange(&self, exchange: &str, trades: &[Trade]) -> anyhow::Result<()>;

    /// Store liquidity depth statistics
    async fn store_liquidity_depth(&self, depth_stats: &[LiquidityDepthStats]) -> anyhow::Result<()>;

    // Query methods

    /// Get tickers for a symbol from a specific exchange within a time range
    async fn get_tickers(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Ticker>>;

    /// Get latest ticker for a symbol from a specific exchange
    async fn get_latest_ticker(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<Ticker>>;

    /// Get klines for a symbol from a specific exchange within a time range
    async fn get_klines(
        &self,
        exchange: &str,
        symbol: &str,
        interval: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Kline>>;

    /// Get latest kline for a symbol from a specific exchange with specific interval
    async fn get_latest_kline(&self, exchange: &str, symbol: &str, interval: &str) -> anyhow::Result<Option<Kline>>;

    /// Get trades for a symbol from a specific exchange within a time range
    async fn get_trades(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Trade>>;

    /// Get funding rates for a symbol from a specific exchange within a time range
    async fn get_funding_rates(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<FundingRate>>;

    /// Get latest funding rate for a symbol from a specific exchange
    async fn get_latest_funding_rate(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<FundingRate>>;

    /// Get orderbooks for a symbol from a specific exchange within a time range
    async fn get_orderbooks(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Orderbook>>;

    /// Get latest orderbook for a symbol from a specific exchange
    async fn get_latest_orderbook(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<Orderbook>>;

    /// Get liquidity depth statistics for a symbol from a specific exchange within a time range
    async fn get_liquidity_depth(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<LiquidityDepthStats>>;

    /// Get latest liquidity depth for a symbol from a specific exchange
    async fn get_latest_liquidity_depth(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<LiquidityDepthStats>>;

    /// Get open interest for a symbol from a specific exchange within a time range
    async fn get_open_interest(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<OpenInterest>>;

    /// Get latest open interest for a symbol from a specific exchange
    async fn get_latest_open_interest(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<OpenInterest>>;
}

/// PostgreSQL implementation of the Repository trait
pub struct PostgresRepository {
    pool: PgPool,
}

impl PostgresRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Helper method to get exchange_id from exchange name
    async fn get_exchange_id(&self, exchange_name: &str) -> anyhow::Result<i32> {
        let row: (i32,) = sqlx::query_as(
            "SELECT id FROM exchanges WHERE name = $1"
        )
        .bind(exchange_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Ensure partition exists for a given date and table
    /// Creates the partition if it doesn't exist
    pub async fn ensure_partition(&self, table: &str, date: NaiveDate) -> anyhow::Result<()> {
        let partition_name = format!("{}_{}", table, date.format("%Y_%m_%d"));
        let next_date = date.succ_opt().ok_or_else(|| anyhow::anyhow!("Failed to calculate next date"))?;

        // Check if partition exists
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_class WHERE relname = $1)"
        )
        .bind(&partition_name)
        .fetch_one(&self.pool)
        .await?;

        if !exists {
            let query = format!(
                "CREATE TABLE IF NOT EXISTS {} PARTITION OF {} FOR VALUES FROM ('{}') TO ('{}')",
                partition_name,
                table,
                date.format("%Y-%m-%d"),
                next_date.format("%Y-%m-%d")
            );

            sqlx::query(&query).execute(&self.pool).await?;
            tracing::debug!("✓ Created partition: {}", partition_name);
        }

        Ok(())
    }

    /// Ensure partitions exist for a timestamp range
    /// This method creates partitions for all dates within the range
    pub async fn ensure_partitions_for_range(
        &self,
        table: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let start_date = start.date_naive();
        let end_date = end.date_naive();

        let mut current_date = start_date;
        while current_date <= end_date {
            self.ensure_partition(table, current_date).await?;
            current_date = current_date.succ_opt().ok_or_else(|| anyhow::anyhow!("Failed to calculate next date"))?;
        }

        Ok(())
    }

    /// Drop old partitions older than the specified number of days
    /// This method helps manage disk space by removing historical data
    pub async fn cleanup_old_partitions(&self, retention_days: i64) -> anyhow::Result<usize> {
        let tables = vec!["tickers", "orderbooks", "trades", "funding_rates", "liquidity_depth", "klines"];

        // Calculate cutoff date manually
        let today = Utc::now().date_naive();
        let mut cutoff_date = today;
        for _ in 0..retention_days {
            cutoff_date = cutoff_date.pred_opt()
                .ok_or_else(|| anyhow::anyhow!("Failed to calculate cutoff date"))?;
        }

        let mut dropped_count = 0;

        for table in &tables {
            // Find all partitions for this table that are older than cutoff
            let query = format!(
                "SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND tablename LIKE '{}_%' ORDER BY tablename",
                table
            );

            let rows: Vec<(String,)> = sqlx::query_as(&query)
                .fetch_all(&self.pool)
                .await?;

            for (partition_name,) in rows {
                // Extract date from partition name (format: table_YYYY_MM_DD)
                if let Some(date_str) = partition_name.strip_prefix(&format!("{}_", table)) {
                    // Parse date from YYYY_MM_DD format
                    let parts: Vec<&str> = date_str.split('_').collect();
                    if parts.len() == 3 {
                        if let (Ok(year), Ok(month), Ok(day)) = (
                            parts[0].parse::<i32>(),
                            parts[1].parse::<u32>(),
                            parts[2].parse::<u32>(),
                        ) {
                            if let Some(partition_date) = NaiveDate::from_ymd_opt(year, month, day) {
                                if partition_date < cutoff_date {
                                    // Drop this partition
                                    let drop_query = format!("DROP TABLE IF EXISTS {} CASCADE", partition_name);
                                    sqlx::query(&drop_query).execute(&self.pool).await?;
                                    tracing::info!("Dropped old partition: {}", partition_name);
                                    dropped_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(dropped_count)
    }

    /// Get statistics about partition counts per table
    pub async fn get_partition_stats(&self) -> anyhow::Result<Vec<(String, usize)>> {
        let tables = vec!["tickers", "orderbooks", "trades", "funding_rates", "liquidity_depth", "klines"];
        let mut stats = Vec::new();

        for table in &tables {
            let query = format!(
                "SELECT COUNT(*) FROM pg_tables WHERE schemaname = 'public' AND tablename LIKE '{}_%'",
                table
            );

            let count: (i64,) = sqlx::query_as(&query)
                .fetch_one(&self.pool)
                .await?;

            stats.push((table.to_string(), count.0 as usize));
        }

        Ok(stats)
    }
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn store_markets(&self, _markets: &[Market]) -> anyhow::Result<()> {
        tracing::warn!("store_markets not yet implemented - markets require exchange context, use direct storage with exchange_id");
        Ok(())
    }

    async fn store_tickers(&self, _tickers: &[Ticker]) -> anyhow::Result<()> {
        tracing::warn!("store_tickers not yet implemented - use store_tickers_with_exchange instead");
        Ok(())
    }

    async fn store_tickers_with_exchange(&self, exchange: &str, tickers: &[Ticker]) -> anyhow::Result<()> {
        if tickers.is_empty() {
            return Ok(());
        }

        let exchange_id = self.get_exchange_id(exchange).await?;
        let mut tx = self.pool.begin().await?;

        for ticker in tickers {
            // Normalize symbol to global format (e.g., BTCUSDT -> BTC)
            let normalized_symbol = extract_base_symbol(&ticker.symbol);

            sqlx::query(
                r#"
                INSERT INTO tickers (
                    exchange_id, symbol, last_price, mark_price, index_price,
                    best_bid_price, best_bid_qty, best_ask_price, best_ask_qty,
                    volume_24h, turnover_24h,
                    price_change_24h, price_change_pct,
                    high_24h, low_24h, ts
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                ON CONFLICT DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(ticker.last_price)
            .bind(ticker.mark_price)
            .bind(ticker.index_price)
            .bind(ticker.best_bid_price)
            .bind(ticker.best_bid_qty)
            .bind(ticker.best_ask_price)
            .bind(ticker.best_ask_qty)
            .bind(ticker.volume_24h)
            .bind(ticker.turnover_24h)
            .bind(ticker.price_change_24h)
            .bind(ticker.price_change_pct)
            .bind(ticker.high_price_24h)
            .bind(ticker.low_price_24h)
            .bind(ticker.timestamp)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!("✓ Stored {} ticker records to database for exchange {}", tickers.len(), exchange);
        Ok(())
    }

    async fn store_orderbooks(&self, _orderbooks: &[Orderbook]) -> anyhow::Result<()> {
        tracing::warn!("store_orderbooks not yet implemented - orderbooks require exchange context, use store_orderbooks_with_exchange instead");
        Ok(())
    }

    async fn store_orderbooks_with_exchange(&self, exchange: &str, orderbooks: &[Orderbook]) -> anyhow::Result<()> {
        if orderbooks.is_empty() {
            return Ok(());
        }

        let exchange_id = self.get_exchange_id(exchange).await?;
        let mut tx = self.pool.begin().await?;

        for orderbook in orderbooks {
            // Normalize symbol to global format
            let normalized_symbol = extract_base_symbol(&orderbook.symbol);

            // Calculate spread in basis points
            let spread_bps = if !orderbook.bids.is_empty() && !orderbook.asks.is_empty() {
                let best_bid = orderbook.bids[0].price;
                let best_ask = orderbook.asks[0].price;
                let spread = best_ask - best_bid;
                let mid = (best_bid + best_ask) / rust_decimal::Decimal::from(2);
                if mid > rust_decimal::Decimal::ZERO {
                    ((spread / mid) * rust_decimal::Decimal::from(10000)).to_i32().unwrap_or(0)
                } else {
                    0
                }
            } else {
                0
            };

            // Convert orderbook to JSON
            let raw_book = serde_json::to_value(orderbook)?;

            sqlx::query(
                r#"
                INSERT INTO orderbooks (
                    exchange_id, symbol, spread_bps, raw_book, ts
                )
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(spread_bps)
            .bind(raw_book)
            .bind(orderbook.timestamp)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!("Stored {} orderbook records to database for exchange {}", orderbooks.len(), exchange);
        Ok(())
    }

    async fn store_funding_rates(&self, _rates: &[FundingRate]) -> anyhow::Result<()> {
        tracing::warn!("store_funding_rates not yet implemented - use store_funding_rates_with_exchange instead");
        Ok(())
    }

    async fn store_funding_rates_with_exchange(&self, exchange: &str, rates: &[FundingRate]) -> anyhow::Result<()> {
        if rates.is_empty() {
            return Ok(());
        }

        let exchange_id = self.get_exchange_id(exchange).await?;
        let mut tx = self.pool.begin().await?;

        for rate in rates {
            // Normalize symbol to global format
            let normalized_symbol = extract_base_symbol(&rate.symbol);

            sqlx::query(
                r#"
                INSERT INTO funding_rates (
                    exchange_id, symbol, rate, next_rate, ts
                )
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(rate.funding_rate)
            .bind(rate.predicted_rate)
            .bind(rate.funding_time)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!("✓ Stored {} funding rate records to database for exchange {}", rates.len(), exchange);
        Ok(())
    }

    async fn store_open_interest(&self, _oi: &[OpenInterest]) -> anyhow::Result<()> {
        tracing::warn!("store_open_interest not yet implemented - use store_open_interest_with_exchange instead");
        Ok(())
    }

    async fn store_open_interest_with_exchange(&self, exchange: &str, oi: &[OpenInterest]) -> anyhow::Result<()> {
        if oi.is_empty() {
            return Ok(());
        }

        let exchange_id = self.get_exchange_id(exchange).await?;
        let mut tx = self.pool.begin().await?;

        for interest in oi {
            // Normalize symbol to global format
            let normalized_symbol = extract_base_symbol(&interest.symbol);

            sqlx::query(
                r#"
                INSERT INTO open_interest (
                    exchange_id, symbol, open_interest, open_value, ts
                )
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(interest.open_interest)
            .bind(interest.open_value)
            .bind(interest.timestamp)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!("✓ Stored {} open interest records to database for exchange {}", oi.len(), exchange);
        Ok(())
    }

    async fn store_klines(&self, _klines: &[Kline]) -> anyhow::Result<()> {
        tracing::warn!("store_klines not yet implemented - klines require exchange context, use store_klines_with_exchange instead");
        Ok(())
    }

    async fn store_klines_with_exchange(&self, exchange: &str, klines: &[Kline]) -> anyhow::Result<()> {
        if klines.is_empty() {
            return Ok(());
        }

        let exchange_id = self.get_exchange_id(exchange).await?;
        let mut tx = self.pool.begin().await?;

        for kline in klines {
            // Normalize symbol to global format
            let normalized_symbol = extract_base_symbol(&kline.symbol);

            sqlx::query(
                r#"
                INSERT INTO klines (
                    exchange_id, symbol, interval,
                    open_time, close_time,
                    open_price, high_price, low_price, close_price,
                    volume, quote_volume, trade_count
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                ON CONFLICT (exchange_id, symbol, interval, open_time) DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(&kline.interval)
            .bind(kline.open_time)
            .bind(kline.close_time)
            .bind(kline.open)
            .bind(kline.high)
            .bind(kline.low)
            .bind(kline.close)
            .bind(kline.volume)
            .bind(kline.turnover)  // Use turnover as quote_volume
            .bind(Some(0))  // trade_count is not in Kline, set to 0
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!("✓ Stored {} kline records to database for exchange {}", klines.len(), exchange);
        Ok(())
    }

    async fn store_trades(&self, _trades: &[Trade]) -> anyhow::Result<()> {
        tracing::warn!("store_trades not yet implemented - trades require exchange context, use store_trades_with_exchange instead");
        Ok(())
    }

    async fn store_trades_with_exchange(&self, exchange: &str, trades: &[Trade]) -> anyhow::Result<()> {
        if trades.is_empty() {
            return Ok(());
        }

        let exchange_id = self.get_exchange_id(exchange).await?;
        let mut tx = self.pool.begin().await?;

        for trade in trades {
            // Normalize symbol to global format
            let normalized_symbol = extract_base_symbol(&trade.symbol);

            let side_str = match trade.side {
                OrderSide::Buy => "buy",
                OrderSide::Sell => "sell",
            };

            sqlx::query(
                r#"
                INSERT INTO trades (
                    exchange_id, symbol, trade_id, price, size, side, ts
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(&trade.id)
            .bind(trade.price)
            .bind(trade.quantity)
            .bind(side_str)
            .bind(trade.timestamp)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        tracing::info!("Stored {} trade records to database for exchange {}", trades.len(), exchange);
        Ok(())
    }

    async fn store_liquidity_depth(&self, depth_stats: &[LiquidityDepthStats]) -> anyhow::Result<()> {
        if depth_stats.is_empty() {
            return Ok(());
        }

        // Begin a transaction for batch insert
        let mut tx = self.pool.begin().await?;

        for stat in depth_stats {
            // Get exchange_id from exchange name
            let exchange_id = self.get_exchange_id(&stat.exchange).await?;

            // Normalize symbol to global format
            let normalized_symbol = extract_base_symbol(&stat.symbol);

            // Insert liquidity depth data
            // Using INSERT ... ON CONFLICT DO NOTHING for idempotency
            sqlx::query(
                r#"
                INSERT INTO liquidity_depth (
                    exchange_id, symbol, mid_price,
                    bid_1bps, bid_2_5bps, bid_5bps, bid_10bps, bid_20bps,
                    ask_1bps, ask_2_5bps, ask_5bps, ask_10bps, ask_20bps,
                    ts
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                ON CONFLICT DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(stat.mid_price)
            .bind(stat.bid_1bps)
            .bind(stat.bid_2_5bps)
            .bind(stat.bid_5bps)
            .bind(stat.bid_10bps)
            .bind(stat.bid_20bps)
            .bind(stat.ask_1bps)
            .bind(stat.ask_2_5bps)
            .bind(stat.ask_5bps)
            .bind(stat.ask_10bps)
            .bind(stat.ask_20bps)
            .bind(stat.timestamp)
            .execute(&mut *tx)
            .await?;
        }

        // Commit the transaction
        tx.commit().await?;

        tracing::info!("✓ Stored {} liquidity depth records to database", depth_stats.len());
        Ok(())
    }

    // Query method implementations

    async fn get_tickers(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Ticker>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let query = if let Some(limit_val) = limit {
            format!(
                r#"
                SELECT last_price, mark_price, index_price,
                       best_bid, best_bid_qty, best_ask, best_ask_qty,
                       volume_24h, turnover_24h, price_change_24h, price_change_pct,
                       high_24h, low_24h, ts
                FROM tickers
                WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
                ORDER BY ts DESC
                LIMIT {}
                "#,
                limit_val
            )
        } else {
            r#"
            SELECT last_price, mark_price, index_price,
                   best_bid, best_bid_qty, best_ask, best_ask_qty,
                   volume_24h, turnover_24h, price_change_24h, price_change_pct,
                   high_24h, low_24h, ts
            FROM tickers
            WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
            ORDER BY ts DESC
            "#
            .to_string()
        };

        let rows = sqlx::query(&query)
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        let mut tickers = Vec::new();
        for row in rows {
            tickers.push(Ticker {
                symbol: symbol.to_string(),
                last_price: row.get("last_price"),
                mark_price: row.get("mark_price"),
                index_price: row.get("index_price"),
                best_bid_price: row.get("best_bid"),
                best_bid_qty: row.try_get("best_bid_qty").unwrap_or(rust_decimal::Decimal::ZERO),
                best_ask_price: row.get("best_ask"),
                best_ask_qty: row.try_get("best_ask_qty").unwrap_or(rust_decimal::Decimal::ZERO),
                volume_24h: row.get("volume_24h"),
                turnover_24h: row.get("turnover_24h"),
                price_change_24h: row.get("price_change_24h"),
                price_change_pct: row.try_get("price_change_pct").unwrap_or(rust_decimal::Decimal::ZERO),
                high_price_24h: row.get("high_24h"),
                low_price_24h: row.get("low_24h"),
                timestamp: row.get("ts"),
            });
        }

        Ok(tickers)
    }

    async fn get_latest_ticker(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<Ticker>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let row = sqlx::query(
            r#"
            SELECT last_price, mark_price, index_price,
                   best_bid, best_bid_qty, best_ask, best_ask_qty,
                   volume_24h, turnover_24h, price_change_24h, price_change_pct,
                   high_24h, low_24h, ts
            FROM tickers
            WHERE exchange_id = $1 AND symbol = $2
            ORDER BY ts DESC
            LIMIT 1
            "#
        )
        .bind(exchange_id)
        .bind(&normalized_symbol)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Ticker {
            symbol: symbol.to_string(),
            last_price: r.get("last_price"),
            mark_price: r.get("mark_price"),
            index_price: r.get("index_price"),
            best_bid_price: r.get("best_bid"),
            best_bid_qty: r.try_get("best_bid_qty").unwrap_or(rust_decimal::Decimal::ZERO),
            best_ask_price: r.get("best_ask"),
            best_ask_qty: r.try_get("best_ask_qty").unwrap_or(rust_decimal::Decimal::ZERO),
            volume_24h: r.get("volume_24h"),
            turnover_24h: r.get("turnover_24h"),
            price_change_24h: r.get("price_change_24h"),
            price_change_pct: r.try_get("price_change_pct").unwrap_or(rust_decimal::Decimal::ZERO),
            high_price_24h: r.get("high_24h"),
            low_price_24h: r.get("low_24h"),
            timestamp: r.get("ts"),
        }))
    }

    async fn get_klines(
        &self,
        exchange: &str,
        symbol: &str,
        interval: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Kline>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let query = if let Some(limit_val) = limit {
            format!(
                r#"
                SELECT interval, open_time, close_time, open_price, high_price, low_price,
                       close_price, volume, quote_volume, trade_count, ts
                FROM klines
                WHERE exchange_id = $1 AND symbol = $2 AND interval = $3 AND ts >= $4 AND ts <= $5
                ORDER BY open_time DESC
                LIMIT {}
                "#,
                limit_val
            )
        } else {
            r#"
            SELECT interval, open_time, close_time, open_price, high_price, low_price,
                   close_price, volume, quote_volume, trade_count, ts
            FROM klines
            WHERE exchange_id = $1 AND symbol = $2 AND interval = $3 AND ts >= $4 AND ts <= $5
            ORDER BY open_time DESC
            "#
            .to_string()
        };

        let rows = sqlx::query(&query)
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(interval)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        let mut klines = Vec::new();
        for row in rows {
            klines.push(Kline {
                symbol: symbol.to_string(),
                interval: row.get("interval"),
                open_time: row.get("open_time"),
                close_time: row.get("close_time"),
                open: row.get("open_price"),
                high: row.get("high_price"),
                low: row.get("low_price"),
                close: row.get("close_price"),
                volume: row.get("volume"),
                turnover: row.get("quote_volume"),  // Map quote_volume to turnover
            });
        }

        Ok(klines)
    }

    async fn get_latest_kline(&self, exchange: &str, symbol: &str, interval: &str) -> anyhow::Result<Option<Kline>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let row = sqlx::query(
            r#"
            SELECT interval, open_time, close_time, open_price, high_price, low_price,
                   close_price, volume, quote_volume
            FROM klines
            WHERE exchange_id = $1 AND symbol = $2 AND interval = $3
            ORDER BY open_time DESC
            LIMIT 1
            "#
        )
        .bind(exchange_id)
        .bind(&normalized_symbol)
        .bind(interval)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Kline {
            symbol: symbol.to_string(),
            interval: r.get("interval"),
            open_time: r.get("open_time"),
            close_time: r.get("close_time"),
            open: r.get("open_price"),
            high: r.get("high_price"),
            low: r.get("low_price"),
            close: r.get("close_price"),
            volume: r.get("volume"),
            turnover: r.get("quote_volume"),
        }))
    }

    async fn get_trades(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Trade>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let query = if let Some(limit_val) = limit {
            format!(
                r#"
                SELECT trade_id, price, size, side, ts
                FROM trades
                WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
                ORDER BY ts DESC
                LIMIT {}
                "#,
                limit_val
            )
        } else {
            r#"
            SELECT trade_id, price, size, side, ts
            FROM trades
            WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
            ORDER BY ts DESC
            "#
            .to_string()
        };

        let rows = sqlx::query(&query)
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        let mut trades = Vec::new();
        for row in rows {
            let side_str: String = row.get("side");
            trades.push(Trade {
                id: row.get("trade_id"),
                symbol: symbol.to_string(),
                price: row.get("price"),
                quantity: row.get("size"),
                side: match side_str.as_str() {
                    "buy" => OrderSide::Buy,
                    "sell" => OrderSide::Sell,
                    _ => OrderSide::Buy,
                },
                timestamp: row.get("ts"),
            });
        }

        Ok(trades)
    }

    async fn get_funding_rates(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<FundingRate>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let query = if let Some(limit_val) = limit {
            format!(
                r#"
                SELECT rate, next_rate, ts
                FROM funding_rates
                WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
                ORDER BY ts DESC
                LIMIT {}
                "#,
                limit_val
            )
        } else {
            r#"
            SELECT rate, next_rate, ts
            FROM funding_rates
            WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
            ORDER BY ts DESC
            "#
            .to_string()
        };

        let rows = sqlx::query(&query)
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        let mut rates = Vec::new();
        for row in rows {
            let ts: DateTime<Utc> = row.get("ts");
            rates.push(FundingRate {
                symbol: symbol.to_string(),
                funding_rate: row.get("rate"),
                predicted_rate: row.get("next_rate"),
                funding_time: ts,
                next_funding_time: ts,  // Default to same as funding_time
                funding_interval: 8,  // Default 8 hours
                funding_rate_cap_floor: rust_decimal::Decimal::ZERO,  // Default to 0
            });
        }

        Ok(rates)
    }

    async fn get_latest_funding_rate(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<FundingRate>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let row = sqlx::query(
            r#"
            SELECT rate, next_rate, ts
            FROM funding_rates
            WHERE exchange_id = $1 AND symbol = $2
            ORDER BY ts DESC
            LIMIT 1
            "#
        )
        .bind(exchange_id)
        .bind(&normalized_symbol)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let ts: DateTime<Utc> = r.get("ts");
            FundingRate {
                symbol: symbol.to_string(),
                funding_rate: r.get("rate"),
                predicted_rate: r.get("next_rate"),
                funding_time: ts,
                next_funding_time: ts,  // Default to same as funding_time
                funding_interval: 8,  // Default 8 hours
                funding_rate_cap_floor: rust_decimal::Decimal::ZERO,  // Default to 0
            }
        }))
    }

    async fn get_orderbooks(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<Orderbook>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let query = if let Some(limit_val) = limit {
            format!(
                r#"
                SELECT raw_book, ts
                FROM orderbooks
                WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
                ORDER BY ts DESC
                LIMIT {}
                "#,
                limit_val
            )
        } else {
            r#"
            SELECT raw_book, ts
            FROM orderbooks
            WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
            ORDER BY ts DESC
            "#
            .to_string()
        };

        let rows = sqlx::query(&query)
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        let mut orderbooks = Vec::new();
        for row in rows {
            let raw_book: serde_json::Value = row.get("raw_book");
            if let Ok(orderbook) = serde_json::from_value::<Orderbook>(raw_book) {
                orderbooks.push(orderbook);
            }
        }

        Ok(orderbooks)
    }

    async fn get_latest_orderbook(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<Orderbook>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let row = sqlx::query(
            r#"
            SELECT raw_book, ts
            FROM orderbooks
            WHERE exchange_id = $1 AND symbol = $2
            ORDER BY ts DESC
            LIMIT 1
            "#
        )
        .bind(exchange_id)
        .bind(&normalized_symbol)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            let raw_book: serde_json::Value = r.get("raw_book");
            serde_json::from_value::<Orderbook>(raw_book).ok()
        }))
    }

    async fn get_liquidity_depth(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<LiquidityDepthStats>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let query = if let Some(limit_val) = limit {
            format!(
                r#"
                SELECT mid_price, bid_1bps, bid_2_5bps, bid_5bps, bid_10bps, bid_20bps,
                       ask_1bps, ask_2_5bps, ask_5bps, ask_10bps, ask_20bps, ts
                FROM liquidity_depth
                WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
                ORDER BY ts DESC
                LIMIT {}
                "#,
                limit_val
            )
        } else {
            r#"
            SELECT mid_price, bid_1bps, bid_2_5bps, bid_5bps, bid_10bps, bid_20bps,
                   ask_1bps, ask_2_5bps, ask_5bps, ask_10bps, ask_20bps, ts
            FROM liquidity_depth
            WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
            ORDER BY ts DESC
            "#
            .to_string()
        };

        let rows = sqlx::query(&query)
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        let mut stats = Vec::new();
        for row in rows {
            stats.push(LiquidityDepthStats {
                exchange: exchange.to_string(),
                symbol: symbol.to_string(),
                mid_price: row.get("mid_price"),
                bid_1bps: row.get("bid_1bps"),
                bid_2_5bps: row.get("bid_2_5bps"),
                bid_5bps: row.get("bid_5bps"),
                bid_10bps: row.get("bid_10bps"),
                bid_20bps: row.get("bid_20bps"),
                ask_1bps: row.get("ask_1bps"),
                ask_2_5bps: row.get("ask_2_5bps"),
                ask_5bps: row.get("ask_5bps"),
                ask_10bps: row.get("ask_10bps"),
                ask_20bps: row.get("ask_20bps"),
                timestamp: row.get("ts"),
            });
        }

        Ok(stats)
    }

    async fn get_latest_liquidity_depth(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<LiquidityDepthStats>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let row = sqlx::query(
            r#"
            SELECT mid_price, bid_1bps, bid_2_5bps, bid_5bps, bid_10bps, bid_20bps,
                   ask_1bps, ask_2_5bps, ask_5bps, ask_10bps, ask_20bps, ts
            FROM liquidity_depth
            WHERE exchange_id = $1 AND symbol = $2
            ORDER BY ts DESC
            LIMIT 1
            "#
        )
        .bind(exchange_id)
        .bind(&normalized_symbol)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| LiquidityDepthStats {
            exchange: exchange.to_string(),
            symbol: symbol.to_string(),
            mid_price: r.get("mid_price"),
            bid_1bps: r.get("bid_1bps"),
            bid_2_5bps: r.get("bid_2_5bps"),
            bid_5bps: r.get("bid_5bps"),
            bid_10bps: r.get("bid_10bps"),
            bid_20bps: r.get("bid_20bps"),
            ask_1bps: r.get("ask_1bps"),
            ask_2_5bps: r.get("ask_2_5bps"),
            ask_5bps: r.get("ask_5bps"),
            ask_10bps: r.get("ask_10bps"),
            ask_20bps: r.get("ask_20bps"),
            timestamp: r.get("ts"),
        }))
    }

    async fn get_open_interest(
        &self,
        exchange: &str,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: Option<i64>,
    ) -> anyhow::Result<Vec<OpenInterest>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let query = if let Some(limit_val) = limit {
            format!(
                r#"
                SELECT open_interest, open_value, ts
                FROM open_interest
                WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
                ORDER BY ts DESC
                LIMIT {}
                "#,
                limit_val
            )
        } else {
            r#"
            SELECT open_interest, open_value, ts
            FROM open_interest
            WHERE exchange_id = $1 AND symbol = $2 AND ts >= $3 AND ts <= $4
            ORDER BY ts DESC
            "#
            .to_string()
        };

        let rows = sqlx::query(&query)
            .bind(exchange_id)
            .bind(&normalized_symbol)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        let mut oi = Vec::new();
        for row in rows {
            oi.push(OpenInterest {
                symbol: symbol.to_string(),
                open_interest: row.get("open_interest"),
                open_value: row.get("open_value"),
                timestamp: row.get("ts"),
            });
        }

        Ok(oi)
    }

    async fn get_latest_open_interest(&self, exchange: &str, symbol: &str) -> anyhow::Result<Option<OpenInterest>> {
        let exchange_id = self.get_exchange_id(exchange).await?;
        let normalized_symbol = extract_base_symbol(symbol);

        let row = sqlx::query(
            r#"
            SELECT open_interest, open_value, ts
            FROM open_interest
            WHERE exchange_id = $1 AND symbol = $2
            ORDER BY ts DESC
            LIMIT 1
            "#
        )
        .bind(exchange_id)
        .bind(&normalized_symbol)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| OpenInterest {
            symbol: symbol.to_string(),
            open_interest: r.get("open_interest"),
            open_value: r.get("open_value"),
            timestamp: r.get("ts"),
        }))
    }
}
