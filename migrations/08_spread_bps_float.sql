-- spread_bps was INTEGER, losing sub-bps precision for high-price assets like BTC.
-- Change to DOUBLE PRECISION to preserve fractional basis points.
ALTER TABLE orderbooks
    ALTER COLUMN spread_bps TYPE DOUBLE PRECISION
    USING spread_bps::double precision;
