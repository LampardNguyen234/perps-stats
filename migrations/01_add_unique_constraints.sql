-- Add unique constraints to orderbooks and liquidity_depth tables
-- so that ON CONFLICT DO NOTHING actually prevents duplicate rows.
-- Partitioned tables require the partition key (ts) in any unique constraint.

ALTER TABLE orderbooks
    ADD CONSTRAINT orderbooks_unique UNIQUE (exchange_id, symbol, ts);

ALTER TABLE liquidity_depth
    ADD CONSTRAINT liquidity_depth_unique UNIQUE (exchange_id, symbol, ts);
