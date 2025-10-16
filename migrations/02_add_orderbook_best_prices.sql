-- Add best bid/ask price and quantity columns to orderbooks table
-- These store the top-of-book values for quick access without parsing raw_book JSONB

ALTER TABLE orderbooks
ADD COLUMN IF NOT EXISTS best_bid NUMERIC,
ADD COLUMN IF NOT EXISTS best_ask NUMERIC,
ADD COLUMN IF NOT EXISTS best_bid_qty NUMERIC,
ADD COLUMN IF NOT EXISTS best_ask_qty NUMERIC;

-- Add comments
COMMENT ON COLUMN orderbooks.best_bid IS 'Best (highest) bid price';
COMMENT ON COLUMN orderbooks.best_ask IS 'Best (lowest) ask price';
COMMENT ON COLUMN orderbooks.best_bid_qty IS 'Quantity available at best bid price';
COMMENT ON COLUMN orderbooks.best_ask_qty IS 'Quantity available at best ask price';
