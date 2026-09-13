-- StandX (perps.standx.com) is a perpetual futures DEX (crypto + equities/commodities), quote asset DUSD.
-- Fees live-verified 2026-09-13 via query_symbol_info for BTC-USD and XAU-USD (commodity pairs
-- share the same schedule as crypto pairs): maker 0.01%, taker 0.04%.
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('standx', 0.0001, 0.0004)
ON CONFLICT (name) DO NOTHING;
