-- Arcus (api.arcus.xyz) is a multi-asset perpetual futures DEX (crypto + equities/commodities/indices)
-- Fees: maker 0 bps, taker 0.95 bps (project-specified rate, overrides public Base tier of 2.25 bps)
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('arcus', 0.0000, 0.000095)
ON CONFLICT (name) DO NOTHING;
