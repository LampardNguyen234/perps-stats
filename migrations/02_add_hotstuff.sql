-- Migration: Add Hotstuff exchange
-- Fees: maker -0.014% (-0.000140), taker 0.02% (0.000200)

INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('hotstuff', -0.000140, 0.000200)
ON CONFLICT (name) DO NOTHING;
