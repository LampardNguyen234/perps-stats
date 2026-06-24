-- RISEx: EVM-based perpetuals DEX at api.rise.trade
-- Fees: tier 6 (retail) — maker 0%, taker 1.5%
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('risex', 0.0000, 0.0150)
ON CONFLICT (name) DO NOTHING;
