-- Add Gravity DEX exchange to the exchanges table
-- Gravity is a perpetual futures DEX on Solana with API-based market data access

-- UP: Insert Gravity exchange if it doesn't already exist
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('gravity', -0.00003, 0.00024) -- -0.003% / 0.024% (LV9)
ON CONFLICT (name) DO NOTHING;

