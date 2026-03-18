-- Migration: Add Hibachi exchange
-- Fees: 0% maker and taker (per exchange docs)

INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('hibachi', 0.0, 0.0)
ON CONFLICT (name) DO NOTHING;
