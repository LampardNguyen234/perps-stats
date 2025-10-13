-- ============================================
-- perps-stats Complete Database Schema
-- ============================================
-- Consolidated from all migrations for fresh database setups
-- Generated on: 2025-10-13
--
-- USAGE:
--   This file is for FRESH DATABASE SETUP ONLY (drop/recreate scenario)
--   For production migrations, use: cargo run -- db migrate
--
-- To use this file:
--   1. Drop and recreate database:
--      dropdb perps_stats && createdb perps_stats
--   2. Load schema:
--      psql -U postgres -d perps_stats < schema.sql
--   3. Or use the provided script:
--      ./scripts/fresh_db_setup.sh
--
-- CHANGES:
--   - Updated tickers table: added best_bid_qty, best_ask_qty, price_change_pct
--   - Renamed best_bid/best_ask to best_bid_price/best_ask_price
--   - Removed received_at column from all time-series tables
--   - Added create_partition() function for easier partition management
--   - Simplified initial partition creation
--
-- ============================================

-- ============================================
-- 1. REFERENCE TABLES
-- ============================================

-- Create exchanges table
CREATE TABLE IF NOT EXISTS exchanges (
  id SERIAL PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  created_at TIMESTAMP WITH TIME ZONE DEFAULT now()
);

-- Create index on exchange name
CREATE INDEX IF NOT EXISTS idx_exchanges_name ON exchanges(name);

-- Seed known exchanges
INSERT INTO exchanges (name) VALUES
  ('binance'),
  ('bybit'),
  ('kucoin'),
  ('lighter'),
  ('paradex'),
  ('hyperliquid'),
  ('cryptocom')
ON CONFLICT (name) DO NOTHING;

-- Create markets table
CREATE TABLE IF NOT EXISTS markets (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT NOT NULL REFERENCES exchanges(id) ON DELETE CASCADE,
  symbol TEXT NOT NULL,
  base_currency TEXT,
  quote_currency TEXT,
  contract_type TEXT,
  tick_size NUMERIC,
  lot_size NUMERIC,
  metadata JSONB,
  created_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
  UNIQUE (exchange_id, symbol)
);

-- Create indexes on markets
CREATE INDEX IF NOT EXISTS idx_markets_exchange_id ON markets(exchange_id);
CREATE INDEX IF NOT EXISTS idx_markets_symbol ON markets(symbol);
CREATE INDEX IF NOT EXISTS idx_markets_exchange_symbol ON markets(exchange_id, symbol);

-- ============================================
-- 2. TIME-SERIES TABLES (Partitioned)
-- ============================================

-- Create tickers table (partitioned by timestamp)
CREATE TABLE IF NOT EXISTS tickers (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  last_price NUMERIC,
  mark_price NUMERIC,
  index_price NUMERIC,
  best_bid_price NUMERIC,
  best_bid_qty NUMERIC,
  best_ask_price NUMERIC,
  best_ask_qty NUMERIC,
  volume_24h NUMERIC,
  turnover_24h NUMERIC,
  price_change_24h NUMERIC,
  price_change_pct NUMERIC,
  high_24h NUMERIC,
  low_24h NUMERIC,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create orderbooks table (liquidity snapshots, partitioned by timestamp)
CREATE TABLE IF NOT EXISTS orderbooks (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  bids_notional NUMERIC,
  asks_notional NUMERIC,
  raw_book JSONB,
  spread_bps INTEGER,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create trades table (partitioned by timestamp)
CREATE TABLE IF NOT EXISTS trades (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  trade_id TEXT,
  price NUMERIC NOT NULL,
  size NUMERIC NOT NULL,
  side TEXT,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  raw JSONB,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create funding_rates table (partitioned by timestamp)
CREATE TABLE IF NOT EXISTS funding_rates (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  rate NUMERIC NOT NULL,
  next_rate NUMERIC,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  raw JSONB,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create liquidity_depth table (partitioned by timestamp)
-- This table stores calculated liquidity depth statistics at various basis point spreads
CREATE TABLE IF NOT EXISTS liquidity_depth (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  mid_price NUMERIC NOT NULL,
  bid_1bps NUMERIC NOT NULL,
  bid_2_5bps NUMERIC NOT NULL,
  bid_5bps NUMERIC NOT NULL,
  bid_10bps NUMERIC NOT NULL,
  bid_20bps NUMERIC NOT NULL,
  ask_1bps NUMERIC NOT NULL,
  ask_2_5bps NUMERIC NOT NULL,
  ask_5bps NUMERIC NOT NULL,
  ask_10bps NUMERIC NOT NULL,
  ask_20bps NUMERIC NOT NULL,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create open_interest table (partitioned by timestamp)
-- This table stores open interest snapshots for perpetual futures contracts
CREATE TABLE IF NOT EXISTS open_interest (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  open_interest NUMERIC NOT NULL,
  open_value NUMERIC NOT NULL,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create klines table (OHLCV candlestick data, partitioned by open_time)
CREATE TABLE IF NOT EXISTS klines (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  interval TEXT NOT NULL,
  open_time TIMESTAMP WITH TIME ZONE NOT NULL,
  close_time TIMESTAMP WITH TIME ZONE NOT NULL,
  open_price NUMERIC NOT NULL,
  high_price NUMERIC NOT NULL,
  low_price NUMERIC NOT NULL,
  close_price NUMERIC NOT NULL,
  volume NUMERIC NOT NULL,
  quote_volume NUMERIC NOT NULL,
  trade_count INT,
  ts TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now(),
  PRIMARY KEY (id, open_time),
  UNIQUE (exchange_id, symbol, interval, open_time)
) PARTITION BY RANGE (open_time);

-- ============================================
-- 3. AUDIT TABLES
-- ============================================

-- Create ingest_events audit table
CREATE TABLE IF NOT EXISTS ingest_events (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT,
  symbol TEXT,
  event_type TEXT,
  status TEXT,
  message TEXT,
  ts TIMESTAMP WITH TIME ZONE DEFAULT now()
);

-- Create index on ingest_events for monitoring
CREATE INDEX IF NOT EXISTS idx_ingest_events_ts ON ingest_events(ts DESC);
CREATE INDEX IF NOT EXISTS idx_ingest_events_status ON ingest_events(status);

-- ============================================
-- 4. INDEXES FOR TIME-SERIES TABLES
-- ============================================

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

-- Liquidity depth indexes
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_symbol ON liquidity_depth(symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_exchange_id ON liquidity_depth(exchange_id, ts DESC);

-- Open interest indexes
CREATE INDEX IF NOT EXISTS idx_open_interest_symbol ON open_interest(symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_open_interest_exchange_id ON open_interest(exchange_id, ts DESC);

-- Klines indexes
CREATE INDEX IF NOT EXISTS idx_klines_exchange_symbol ON klines(exchange_id, symbol);
CREATE INDEX IF NOT EXISTS idx_klines_symbol_interval ON klines(symbol, interval);
CREATE INDEX IF NOT EXISTS idx_klines_open_time ON klines(open_time DESC);
CREATE INDEX IF NOT EXISTS idx_klines_ts ON klines(ts DESC);

-- ============================================
-- 5. PARTITION MANAGEMENT FUNCTIONS
-- ============================================

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

-- Simplified function to create a partition for a specific date
-- This is the function used by the Rust code
CREATE OR REPLACE FUNCTION create_partition(
    table_name TEXT,
    partition_date DATE
) RETURNS VOID AS $$
DECLARE
    partition_name TEXT;
    start_date DATE;
    end_date DATE;
BEGIN
    -- Format partition name: table_YYYY_MM_DD
    partition_name := table_name || '_' || to_char(partition_date, 'YYYY_MM_DD');

    -- Set date range (start of day to start of next day)
    start_date := partition_date;
    end_date := partition_date + INTERVAL '1 day';

    -- Create partition if it doesn't exist
    PERFORM create_partition_if_not_exists(
        table_name,
        partition_name,
        start_date,
        end_date
    );
END;
$$ LANGUAGE plpgsql;

-- ============================================
-- 6. CREATE INITIAL PARTITIONS
-- ============================================

-- Create today's partition for all time-series tables
DO $$
DECLARE
    today DATE := CURRENT_DATE;
BEGIN
    -- Tickers (partitioned by ts)
    PERFORM create_partition('tickers', today);

    -- Orderbooks (partitioned by ts)
    PERFORM create_partition('orderbooks', today);

    -- Trades (partitioned by ts)
    PERFORM create_partition('trades', today);

    -- Funding rates (partitioned by ts)
    PERFORM create_partition('funding_rates', today);

    -- Liquidity depth (partitioned by ts)
    PERFORM create_partition('liquidity_depth', today);

    -- Open interest (partitioned by ts)
    PERFORM create_partition('open_interest', today);

    -- Klines (partitioned by open_time)
    PERFORM create_partition('klines', today);
END $$;

-- ============================================
-- 7. VERIFICATION QUERIES
-- ============================================

-- Uncomment to verify the schema was created correctly:
-- SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename;
-- SELECT * FROM exchanges;
-- SELECT COUNT(*) as partition_count FROM pg_tables WHERE tablename ~ '_[0-9]{4}_[0-9]{2}_[0-9]{2}$';
