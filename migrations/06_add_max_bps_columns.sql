-- Migration: 06_add_max_bps_columns.sql
-- Add maximum orderbook depth columns to liquidity_depth table

-- Add max_ask_bps column (nullable for backward compatibility)
ALTER TABLE liquidity_depth
ADD COLUMN IF NOT EXISTS max_ask_bps DECIMAL(10,4) NULL;

-- Add max_bid_bps column (nullable for backward compatibility)
ALTER TABLE liquidity_depth
ADD COLUMN IF NOT EXISTS max_bid_bps DECIMAL(10,4) NULL;

-- Add comments for documentation
COMMENT ON COLUMN liquidity_depth.max_ask_bps IS 'Maximum ask spread in basis points from mid-price (how far the deepest ask extends)';
COMMENT ON COLUMN liquidity_depth.max_bid_bps IS 'Maximum bid spread in basis points from mid-price (how far the deepest bid extends)';

-- Add index for efficient querying on max depth metrics
-- This enables fast queries for exchanges/symbols with specific liquidity depth characteristics
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_max_bps
ON liquidity_depth (exchange_id, symbol, max_ask_bps, max_bid_bps);

-- Add time-series index for trend analysis
-- This enables efficient historical analysis of liquidity depth over time
CREATE INDEX IF NOT EXISTS idx_liquidity_depth_ts_max_bps
ON liquidity_depth (exchange_id, symbol, ts)
WHERE max_ask_bps IS NOT NULL AND max_bid_bps IS NOT NULL;