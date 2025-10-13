-- ============================================
-- perps-stats Complete Database Schema
-- ============================================
-- Consolidated schema for fresh database setups
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
--   - Removed partitioning from all time-series tables
--   - Simplified schema with regular tables
--   - Updated tickers table: added best_bid_qty, best_ask_qty, price_change_pct
--   - Renamed best_bid/best_ask to best_bid_price/best_ask_price
--   - Removed received_at column from all time-series tables
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
-- 2. TIME-SERIES TABLES
-- ============================================

-- Create tickers table
CREATE TABLE IF NOT EXISTS tickers (
  id BIGSERIAL PRIMARY KEY,
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
  ts TIMESTAMP WITH TIME ZONE NOT NULL
);

-- Create orderbooks table (liquidity snapshots)
CREATE TABLE IF NOT EXISTS orderbooks (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  bids_notional NUMERIC,
  asks_notional NUMERIC,
  raw_book JSONB,
  spread_bps INTEGER,
  ts TIMESTAMP WITH TIME ZONE NOT NULL
);

-- Create trades table
CREATE TABLE IF NOT EXISTS trades (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  trade_id TEXT,
  price NUMERIC NOT NULL,
  size NUMERIC NOT NULL,
  side TEXT,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  raw JSONB
);

-- Create funding_rates table
CREATE TABLE IF NOT EXISTS funding_rates (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  rate NUMERIC NOT NULL,
  next_rate NUMERIC,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  raw JSONB
);

-- Create liquidity_depth table
-- This table stores calculated liquidity depth statistics at various basis point spreads
CREATE TABLE IF NOT EXISTS liquidity_depth (
  id BIGSERIAL PRIMARY KEY,
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
  ts TIMESTAMP WITH TIME ZONE NOT NULL
);

-- Create open_interest table
-- This table stores open interest snapshots for perpetual futures contracts
CREATE TABLE IF NOT EXISTS open_interest (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  symbol TEXT NOT NULL,
  open_interest NUMERIC NOT NULL,
  open_value NUMERIC NOT NULL,
  ts TIMESTAMP WITH TIME ZONE NOT NULL
);

-- Create klines table (OHLCV candlestick data)
CREATE TABLE IF NOT EXISTS klines (
  id BIGSERIAL PRIMARY KEY,
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
  UNIQUE (exchange_id, symbol, interval, open_time)
);

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
CREATE INDEX IF NOT EXISTS idx_tickers_exchange_symbol ON tickers (exchange_id, symbol);

-- Orderbooks indexes
CREATE INDEX IF NOT EXISTS idx_orderbooks_symbol_ts ON orderbooks (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_orderbooks_exchange_ts ON orderbooks (exchange_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_orderbooks_exchange_symbol ON orderbooks (exchange_id, symbol);

-- Trades indexes
CREATE INDEX IF NOT EXISTS idx_trades_symbol_ts ON trades (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_trades_exchange_ts ON trades (exchange_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_trades_exchange_symbol ON trades (exchange_id, symbol);

-- Funding rates indexes
CREATE INDEX IF NOT EXISTS idx_funding_symbol_ts ON funding_rates (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_funding_exchange_ts ON funding_rates (exchange_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_funding_exchange_symbol ON funding_rates (exchange_id, symbol);

-- Liquidity depth indexes
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_symbol_ts ON liquidity_depth(symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_exchange_ts ON liquidity_depth(exchange_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_exchange_symbol ON liquidity_depth(exchange_id, symbol);

-- Open interest indexes
CREATE INDEX IF NOT EXISTS idx_open_interest_symbol_ts ON open_interest(symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_open_interest_exchange_ts ON open_interest(exchange_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_open_interest_exchange_symbol ON open_interest(exchange_id, symbol);

-- Klines indexes
CREATE INDEX IF NOT EXISTS idx_klines_exchange_symbol ON klines(exchange_id, symbol);
CREATE INDEX IF NOT EXISTS idx_klines_symbol_interval ON klines(symbol, interval);
CREATE INDEX IF NOT EXISTS idx_klines_open_time ON klines(open_time DESC);
CREATE INDEX IF NOT EXISTS idx_klines_ts ON klines(ts DESC);
CREATE INDEX IF NOT EXISTS idx_klines_exchange_symbol_interval ON klines(exchange_id, symbol, interval);

-- ============================================
-- 5. VERIFICATION QUERIES
-- ============================================

-- Uncomment to verify the schema was created correctly:
-- SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename;
-- SELECT * FROM exchanges;
