-- This migration creates initial partitions for the current date
-- Note: Additional partitions should be created programmatically via the CLI

-- Function to create partitions for a given table and date range
CREATE OR REPLACE FUNCTION create_partition_if_not_exists(
    parent_table TEXT,
    partition_name TEXT,
    start_date DATE,
    end_date DATE
) RETURNS VOID AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = partition_name
    ) THEN
        EXECUTE format(
            'CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
            partition_name,
            parent_table,
            start_date,
            end_date
        );
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Create today's partition for tickers (this will be replaced by dynamic creation)
-- Using a far future date range as placeholder - actual partitions created by CLI
DO $$
DECLARE
    today DATE := CURRENT_DATE;
    tomorrow DATE := CURRENT_DATE + INTERVAL '1 day';
BEGIN
    PERFORM create_partition_if_not_exists(
        'tickers',
        'tickers_' || to_char(today, 'YYYY_MM_DD'),
        today,
        tomorrow
    );

    PERFORM create_partition_if_not_exists(
        'orderbooks',
        'orderbooks_' || to_char(today, 'YYYY_MM_DD'),
        today,
        tomorrow
    );

    PERFORM create_partition_if_not_exists(
        'trades',
        'trades_' || to_char(today, 'YYYY_MM_DD'),
        today,
        tomorrow
    );

    PERFORM create_partition_if_not_exists(
        'funding_rates',
        'funding_rates_' || to_char(today, 'YYYY_MM_DD'),
        today,
        tomorrow
    );
END $$;

-- Create indexes on partitioned tables
-- Note: These are created on the parent table and inherited by partitions

-- Tickers indexes
CREATE INDEX IF NOT EXISTS idx_tickers_symbol_ts ON tickers (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_tickers_exchange_ts ON tickers (exchange_id, ts DESC);

-- Orderbooks indexes
CREATE INDEX IF NOT EXISTS idx_orderbooks_symbol_ts ON orderbooks (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_orderbooks_exchange_ts ON orderbooks (exchange_id, ts DESC);

-- Trades indexes
CREATE INDEX IF NOT EXISTS idx_trades_symbol_ts ON trades (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_trades_exchange_ts ON trades (exchange_id, ts DESC);

-- Funding rates indexes
CREATE INDEX IF NOT EXISTS idx_funding_symbol_ts ON funding_rates (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_funding_exchange_ts ON funding_rates (exchange_id, ts DESC);
