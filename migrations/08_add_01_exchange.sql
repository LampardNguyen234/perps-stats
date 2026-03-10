-- Add 01.xyz (Nord) exchange to the exchanges table
-- 01 is a perpetual futures DEX on Solana powered by the Nord engine

-- UP: Insert 01 exchange if it doesn't already exist
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('01', 0.0, 0.0) -- fees unknown, default to zero
ON CONFLICT (name) DO NOTHING;
