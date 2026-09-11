-- EdgeX (edgex-prod-v2.edgex.exchange) is a multi-asset perpetual futures DEX (crypto + equities/commodities)
-- Fees: maker 0.015%, taker 0.02% (project-specified fee tier, overrides the public
-- defaultMakerFeeRate/defaultTakerFeeRate of 0.04%/0.045% seen on sampled contracts)
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('edgex', 0.00015, 0.0002)
ON CONFLICT (name) DO NOTHING;
