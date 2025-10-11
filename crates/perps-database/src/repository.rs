use async_trait::async_trait;
use perps_core::*;
use sqlx::PgPool;

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

    /// Store trades
    async fn store_trades(&self, trades: &[Trade]) -> anyhow::Result<()>;

    /// Store liquidity depth statistics
    async fn store_liquidity_depth(&self, depth_stats: &[LiquidityDepthStats]) -> anyhow::Result<()>;

    // Query methods would be added here as needed
    // For example:
    // async fn get_klines(&self, symbol: &str, interval: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> anyhow::Result<Vec<Kline>>;
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
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn store_markets(&self, _markets: &[Market]) -> anyhow::Result<()> {
        // TODO: Implement using sqlx::query! with INSERT ... ON CONFLICT DO NOTHING
        tracing::warn!("store_markets not yet implemented");
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
            sqlx::query(
                r#"
                INSERT INTO tickers (
                    exchange_id, symbol, last_price, mark_price, index_price,
                    best_bid, best_ask, volume_24h, turnover_24h,
                    price_change_24h, high_24h, low_24h, ts
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                ON CONFLICT DO NOTHING
                "#
            )
            .bind(exchange_id)
            .bind(&ticker.symbol)
            .bind(ticker.last_price)
            .bind(ticker.mark_price)
            .bind(ticker.index_price)
            .bind(ticker.best_bid_price)
            .bind(ticker.best_ask_price)
            .bind(ticker.volume_24h)
            .bind(ticker.turnover_24h)
            .bind(ticker.price_change_24h)
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
        tracing::warn!("store_orderbooks not yet implemented");
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
            .bind(&rate.symbol)
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
            .bind(&interest.symbol)
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
        tracing::warn!("store_klines not yet implemented");
        Ok(())
    }

    async fn store_trades(&self, _trades: &[Trade]) -> anyhow::Result<()> {
        tracing::warn!("store_trades not yet implemented");
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
            .bind(&stat.symbol)
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
}
