-- Bitget USDT-FUTURES fees (standard VIP0 tier, live-verified against instrument data 2026-10-27).
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('bitget', 0.0002, 0.0006)
ON CONFLICT (name) DO NOTHING;
