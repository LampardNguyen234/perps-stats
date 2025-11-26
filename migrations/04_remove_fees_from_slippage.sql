-- Migration: Remove taker fees from existing slippage data
--
-- Previous behavior: Slippage calculations included taker fees in the stored values
-- New behavior: Slippage stores raw values without fees (fees applied at query time)
--
-- This migration reverses the fee application by subtracting the taker fee amount
-- from the stored slippage values using the fee data from the exchanges table.
--
-- Formula: raw_slippage_bps = fee_adjusted_slippage_bps - (taker_fee * 10000)
-- Example: If slippage_bps was 9 (5 bps raw + 4 bps fee) and taker_fee is 0.0004 (4 bps),
--          then new slippage_bps = 9 - 4 = 5 bps

BEGIN;

-- Step 1: Create a temporary table to store the corrected values
CREATE TEMPORARY TABLE slippage_corrected AS
SELECT
    id,
    exchange_id,
    symbol,
    ts,
    mid_price,
    trade_amount,
    buy_avg_price,
    -- Remove fee from buy slippage: subtract (taker_fee * 10000) bps
    CASE
        WHEN buy_slippage_bps IS NOT NULL THEN
            GREATEST(
                0,  -- Ensure we don't go below zero
                buy_slippage_bps - COALESCE(e.taker_fee * 10000, 0)
            )
        ELSE NULL
    END AS buy_slippage_bps,
    CASE
        WHEN buy_slippage_pct IS NOT NULL THEN
            GREATEST(
                0,
                buy_slippage_pct - COALESCE(e.taker_fee * 100, 0)
            )
        ELSE NULL
    END AS buy_slippage_pct,
    buy_total_cost,
    buy_feasible,
    sell_avg_price,
    -- Remove fee from sell slippage: subtract (taker_fee * 10000) bps
    CASE
        WHEN sell_slippage_bps IS NOT NULL THEN
            GREATEST(
                0,
                sell_slippage_bps - COALESCE(e.taker_fee * 10000, 0)
            )
        ELSE NULL
    END AS sell_slippage_bps,
    CASE
        WHEN sell_slippage_pct IS NOT NULL THEN
            GREATEST(
                0,
                sell_slippage_pct - COALESCE(e.taker_fee * 100, 0)
            )
        ELSE NULL
    END AS sell_slippage_pct,
    sell_total_cost,
    sell_feasible
FROM slippage s
LEFT JOIN exchanges e ON s.exchange_id = e.id;

-- Step 2: Update the slippage table with corrected values
UPDATE slippage
SET
    buy_slippage_bps = sc.buy_slippage_bps,
    buy_slippage_pct = sc.buy_slippage_pct,
    sell_slippage_bps = sc.sell_slippage_bps,
    sell_slippage_pct = sc.sell_slippage_pct
FROM slippage_corrected sc
WHERE slippage.id = sc.id;

-- Step 3: Log the operation (count affected rows)
-- This helps verify the migration was successful
DO $$
DECLARE
    updated_count INTEGER;
BEGIN
    SELECT COUNT(*) INTO updated_count
    FROM slippage
    WHERE buy_slippage_bps IS NOT NULL OR sell_slippage_bps IS NOT NULL;

    RAISE NOTICE 'Migration 04: Updated % slippage records to remove taker fees', updated_count;
END $$;

COMMIT;
