-- Consolidated initial schema migration (native PostgreSQL range partitioning)

-- ============================================
-- 1. REFERENCE TABLES
-- ============================================

CREATE TABLE IF NOT EXISTS exchanges (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    maker_fee NUMERIC(10, 6),
    taker_fee NUMERIC(10, 6),
    created_at TIMESTAMP WITH TIME ZONE DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_exchanges_name ON exchanges(name);

COMMENT ON COLUMN exchanges.maker_fee IS 'Maker fee as a decimal (e.g., 0.0002 for 0.02%)';
COMMENT ON COLUMN exchanges.taker_fee IS 'Taker fee as a decimal (e.g., 0.0004 for 0.04%)';

-- Seed all known exchanges with fees
INSERT INTO exchanges (name, maker_fee, taker_fee) VALUES
    ('binance',     0.0000,   0.000084),   -- 0% / 0.0084% (VIP9)
    ('bybit',       0.0000,   0.0003),     -- 0% / 0.03% (Supreme VIP)
    ('extended',    0.0000,   0.00025),    -- 0% / 0.025% (base users)
    ('kucoin',     -0.00005,  0.0002),     -- -0.005% / 0.02% (LV12)
    ('lighter',     0.0000,   0.0000),     -- 0% / 0% (base users)
    ('paradex',     0.00002,  0.00005),    -- 0.002% / 0.005% (Pro)
    ('hyperliquid', 0.0000,   0.000144),   -- 0% / 0.0144% (Tier 6, Diamond)
    ('aster',       0.0000,   0.00023),    -- 0% / 0.023% (VIP 6)
    ('pacifica',    0.0000,   0.00028),    -- 0% / 0.028% (VIP 3)
    ('nado',       -0.00008,  0.00015),    -- -0.008% / 0.015%
    ('gravity',    -0.00003,  0.00024),    -- -0.003% / 0.024% (LV9)
    ('01',          0.0,      0.0002)      -- 0% / 0.02%
ON CONFLICT (name) DO NOTHING;

-- Markets table (reference, not time-series)
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

CREATE INDEX IF NOT EXISTS idx_markets_exchange_id ON markets(exchange_id);
CREATE INDEX IF NOT EXISTS idx_markets_symbol ON markets(symbol);
CREATE INDEX IF NOT EXISTS idx_markets_exchange_symbol ON markets(exchange_id, symbol);

-- ============================================
-- 2. TIME-SERIES TABLES (partitioned by ts)
-- ============================================
-- Notes:
--   - No BIGSERIAL PRIMARY KEY (partitioned tables require PK to include partition key)
--   - No FK constraints (not supported on partitioned tables)
--   - UNIQUE constraints must include the partition column (ts)
--   - Partitions are created/managed by the application (partitions.rs)

-- Tickers table
CREATE TABLE IF NOT EXISTS tickers (
    exchange_id INT NOT NULL,
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
    open_interest NUMERIC,
    open_interest_notional NUMERIC,
    price_change_24h NUMERIC,
    price_change_pct NUMERIC,
    high_24h NUMERIC,
    low_24h NUMERIC,
    ts TIMESTAMP WITH TIME ZONE NOT NULL
) PARTITION BY RANGE (ts);

-- Orderbooks table (summary only — full book stored in Parquet files)
CREATE TABLE IF NOT EXISTS orderbooks (
    exchange_id INT NOT NULL,
    symbol TEXT NOT NULL,
    bids_notional NUMERIC,
    asks_notional NUMERIC,
    spread_bps INTEGER,
    bid_size INTEGER,
    ask_size INTEGER,
    best_bid NUMERIC,
    best_ask NUMERIC,
    best_bid_qty NUMERIC,
    best_ask_qty NUMERIC,
    ts TIMESTAMP WITH TIME ZONE NOT NULL
) PARTITION BY RANGE (ts);

COMMENT ON COLUMN orderbooks.bid_size IS 'Number of bid levels in the orderbook';
COMMENT ON COLUMN orderbooks.ask_size IS 'Number of ask levels in the orderbook';
COMMENT ON COLUMN orderbooks.bids_notional IS 'Total notional value (price x quantity) of all bids';
COMMENT ON COLUMN orderbooks.asks_notional IS 'Total notional value (price x quantity) of all asks';
COMMENT ON COLUMN orderbooks.spread_bps IS 'Spread between best bid and best ask in basis points';
COMMENT ON COLUMN orderbooks.best_bid IS 'Best (highest) bid price';
COMMENT ON COLUMN orderbooks.best_ask IS 'Best (lowest) ask price';
COMMENT ON COLUMN orderbooks.best_bid_qty IS 'Quantity available at best bid price';
COMMENT ON COLUMN orderbooks.best_ask_qty IS 'Quantity available at best ask price';

-- Trades table
CREATE TABLE IF NOT EXISTS trades (
    exchange_id INT NOT NULL,
    symbol TEXT NOT NULL,
    trade_id TEXT,
    price NUMERIC NOT NULL,
    size NUMERIC NOT NULL,
    side TEXT,
    ts TIMESTAMP WITH TIME ZONE NOT NULL,
    raw JSONB
) PARTITION BY RANGE (ts);

-- Funding rates table
CREATE TABLE IF NOT EXISTS funding_rates (
    exchange_id INT NOT NULL,
    symbol TEXT NOT NULL,
    rate NUMERIC NOT NULL,
    next_rate NUMERIC,
    ts TIMESTAMP WITH TIME ZONE NOT NULL,
    raw JSONB
) PARTITION BY RANGE (ts);

-- Liquidity depth table
CREATE TABLE IF NOT EXISTS liquidity_depth (
    exchange_id INT NOT NULL,
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
    max_ask_bps DECIMAL(10,4) NULL,
    max_bid_bps DECIMAL(10,4) NULL,
    ts TIMESTAMP WITH TIME ZONE NOT NULL
) PARTITION BY RANGE (ts);

COMMENT ON COLUMN liquidity_depth.max_ask_bps IS 'Maximum ask spread in basis points from mid-price (how far the deepest ask extends)';
COMMENT ON COLUMN liquidity_depth.max_bid_bps IS 'Maximum bid spread in basis points from mid-price (how far the deepest bid extends)';

-- Klines table (OHLCV candlestick data)
CREATE TABLE IF NOT EXISTS klines (
    exchange_id INT NOT NULL,
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
    UNIQUE (exchange_id, symbol, interval, open_time, ts)
) PARTITION BY RANGE (ts);

-- Slippage table
CREATE TABLE IF NOT EXISTS slippage (
    exchange_id INTEGER NOT NULL,
    symbol VARCHAR(20) NOT NULL,
    ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    mid_price DECIMAL(20, 5) NOT NULL,
    trade_amount DECIMAL(20, 4) NOT NULL,
    buy_avg_price DECIMAL(20, 8),
    buy_slippage_bps DECIMAL(10, 4),
    buy_slippage_pct DECIMAL(10, 4),
    buy_total_cost DECIMAL(20, 2),
    buy_feasible BOOLEAN NOT NULL,
    sell_avg_price DECIMAL(20, 8),
    sell_slippage_bps DECIMAL(10, 4),
    sell_slippage_pct DECIMAL(10, 4),
    sell_total_cost DECIMAL(20, 2),
    sell_feasible BOOLEAN NOT NULL,
    CONSTRAINT slippage_unique UNIQUE (exchange_id, symbol, ts, trade_amount)
) PARTITION BY RANGE (ts);

COMMENT ON TABLE slippage IS 'Slippage calculations for fixed trade amounts (1K, 10K, 50K, 100K, 500K USD) across exchanges';

-- ============================================
-- 3. DEFAULT PARTITIONS
-- ============================================
-- Default partitions catch any data that doesn't match a date-specific partition.
-- This prevents INSERT failures when a partition hasn't been pre-created yet.

CREATE TABLE IF NOT EXISTS tickers_default PARTITION OF tickers DEFAULT;
CREATE TABLE IF NOT EXISTS orderbooks_default PARTITION OF orderbooks DEFAULT;
CREATE TABLE IF NOT EXISTS trades_default PARTITION OF trades DEFAULT;
CREATE TABLE IF NOT EXISTS funding_rates_default PARTITION OF funding_rates DEFAULT;
CREATE TABLE IF NOT EXISTS liquidity_depth_default PARTITION OF liquidity_depth DEFAULT;
CREATE TABLE IF NOT EXISTS klines_default PARTITION OF klines DEFAULT;
CREATE TABLE IF NOT EXISTS slippage_default PARTITION OF slippage DEFAULT;

-- ============================================
-- 4. NON-PARTITIONED TABLES
-- ============================================

-- Kline discovery cache (small lookup table, not time-series)
CREATE TABLE IF NOT EXISTS kline_discovery_cache (
    id SERIAL PRIMARY KEY,
    exchange_id INTEGER NOT NULL REFERENCES exchanges(id) ON DELETE CASCADE,
    symbol VARCHAR(20) NOT NULL,
    interval VARCHAR(10) NOT NULL,
    earliest_timestamp TIMESTAMPTZ NOT NULL,
    discovered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    api_calls_used INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    UNIQUE(exchange_id, symbol, interval)
);

-- Ingest events audit table
CREATE TABLE IF NOT EXISTS ingest_events (
    id BIGSERIAL PRIMARY KEY,
    exchange_id INT,
    symbol TEXT,
    event_type TEXT,
    status TEXT,
    message TEXT,
    ts TIMESTAMP WITH TIME ZONE DEFAULT now()
);

-- ============================================
-- 5. INDEXES
-- ============================================
-- Composite (exchange_id, symbol, ts DESC) is the primary query pattern.
-- Indexes are defined on the parent table and automatically inherited by partitions.

-- Tickers
CREATE INDEX IF NOT EXISTS idx_tickers_exchange_symbol_ts ON tickers (exchange_id, symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_tickers_symbol_ts ON tickers (symbol, ts DESC);

-- Orderbooks
CREATE INDEX IF NOT EXISTS idx_orderbooks_exchange_symbol_ts ON orderbooks (exchange_id, symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_orderbooks_symbol_ts ON orderbooks (symbol, ts DESC);

-- Trades
CREATE INDEX IF NOT EXISTS idx_trades_exchange_symbol_ts ON trades (exchange_id, symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_trades_symbol_ts ON trades (symbol, ts DESC);

-- Funding rates
CREATE INDEX IF NOT EXISTS idx_funding_exchange_symbol_ts ON funding_rates (exchange_id, symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_funding_symbol_ts ON funding_rates (symbol, ts DESC);

-- Liquidity depth
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_exchange_symbol_ts ON liquidity_depth (exchange_id, symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_symbol_ts ON liquidity_depth (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_max_bps ON liquidity_depth (exchange_id, symbol, max_ask_bps, max_bid_bps);

-- Klines
CREATE INDEX IF NOT EXISTS idx_klines_exchange_symbol_interval_ts ON klines (exchange_id, symbol, interval, ts DESC);
CREATE INDEX IF NOT EXISTS idx_klines_open_time ON klines (open_time DESC);

-- Slippage
CREATE INDEX IF NOT EXISTS idx_slippage_exchange_symbol_ts ON slippage (exchange_id, symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_slippage_symbol_ts ON slippage (symbol, ts DESC);
CREATE INDEX IF NOT EXISTS idx_slippage_trade_amount ON slippage (trade_amount);

-- Kline discovery cache
CREATE INDEX IF NOT EXISTS idx_kline_discovery_cache_lookup ON kline_discovery_cache (exchange_id, symbol, interval);
CREATE INDEX IF NOT EXISTS idx_kline_discovery_cache_discovered_at ON kline_discovery_cache (discovered_at);

-- Ingest events
CREATE INDEX IF NOT EXISTS idx_ingest_events_ts ON ingest_events (ts DESC);
CREATE INDEX IF NOT EXISTS idx_ingest_events_status ON ingest_events (status);
