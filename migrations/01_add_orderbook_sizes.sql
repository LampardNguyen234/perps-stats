-- Add bid_size and ask_size columns to orderbooks table
-- These store the number of price levels (order book depth) for bids and asks

ALTER TABLE orderbooks
ADD COLUMN IF NOT EXISTS bid_size INTEGER,
ADD COLUMN IF NOT EXISTS ask_size INTEGER;

-- Add comment
COMMENT ON COLUMN orderbooks.bid_size IS 'Number of bid levels in the orderbook';
COMMENT ON COLUMN orderbooks.ask_size IS 'Number of ask levels in the orderbook';
COMMENT ON COLUMN orderbooks.bids_notional IS 'Total notional value (price × quantity) of all bids';
COMMENT ON COLUMN orderbooks.asks_notional IS 'Total notional value (price × quantity) of all asks';
COMMENT ON COLUMN orderbooks.spread_bps IS 'Spread between best bid and best ask in basis points';
COMMENT ON COLUMN orderbooks.raw_book IS 'Full orderbook snapshot in JSONB format';
