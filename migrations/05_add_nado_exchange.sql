-- Add Nado exchange to the exchanges table
-- Nado is a decentralized perpetual futures exchange

-- Insert Nado exchange if it doesn't already exist
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('nado', -0.00008, 0.00015)
ON CONFLICT (name) DO NOTHING;