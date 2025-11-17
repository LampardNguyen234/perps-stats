-- Add maker_fee and taker_fee columns to exchanges table
-- These store the trading fees charged by each exchange

ALTER TABLE exchanges
ADD COLUMN IF NOT EXISTS maker_fee NUMERIC(10, 6),
ADD COLUMN IF NOT EXISTS taker_fee NUMERIC(10, 6);

-- Add comments explaining the columns
COMMENT ON COLUMN exchanges.maker_fee IS 'Maker fee as a decimal (e.g., 0.0002 for 0.02%)';
COMMENT ON COLUMN exchanges.taker_fee IS 'Taker fee as a decimal (e.g., 0.0004 for 0.04%)';

-- Update known exchanges with their standard fees (as of 2025)
-- These are base fees and may vary based on user tier/volume

UPDATE exchanges SET maker_fee = 0.0000, taker_fee = 0.000084 WHERE name = 'binance';  -- 0% / 0.0084% (VIP9)
UPDATE exchanges SET maker_fee = 0.0000, taker_fee = 0.0003 WHERE name = 'bybit';     -- 0% / 0.03% (Supreme VIP)
UPDATE exchanges SET maker_fee = 0.0000, taker_fee = 0.00025 WHERE name = 'extended';  -- 0% / 0.025% (base users)
UPDATE exchanges SET maker_fee = -0.00005, taker_fee = 0.0002 WHERE name = 'kucoin';    -- -0.005% / 0.02% (LV12)
UPDATE exchanges SET maker_fee = 0.0000, taker_fee = 0.0000 WHERE name = 'lighter';   -- 0% / 0% (base users)
UPDATE exchanges SET maker_fee = 0.00002, taker_fee = 0.00005 WHERE name = 'paradex';   -- 0.002% / 0.005% (Pro)
UPDATE exchanges SET maker_fee = 0.0000, taker_fee = 0.000144 WHERE name = 'hyperliquid'; -- 0% / 0.0144% (Tier 6, Diamond)
UPDATE exchanges SET maker_fee = 0.0000, taker_fee = 0.00023 WHERE name = 'aster';     -- 0% / 0.023% (VIP 6)
UPDATE exchanges SET maker_fee = 0.0000, taker_fee = 0.00028 WHERE name = 'pacifica';  -- 0% / 0.028% (VIP 3)
