-- Migration: Add QFEX exchange
-- QFEX is an equity perpetuals DEX (equities vs USD, e.g. NVDA-USD, GOOGL-USD)
-- Fees: 0% maker, 0.006% taker (0.00006 as decimal ratio)

INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('qfex', 0.0, 0.00006)
ON CONFLICT (name) DO NOTHING;
