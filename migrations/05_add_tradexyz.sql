-- Migration: Add tradexyz exchange
-- tradexyz is an HIP-3 equity/RWA perpetuals DEX on Hyperliquid
-- Fees: 0% maker, 0.00228% taker (0.0000228 as decimal ratio)

INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('tradexyz', 0.0, 0.0000228)
ON CONFLICT (name) DO NOTHING;
