-- Create table to cache earliest kline discovery results
-- This avoids re-discovering the same symbol/interval on subsequent backfill runs
CREATE TABLE IF NOT EXISTS kline_discovery_cache (
    id SERIAL PRIMARY KEY,
    exchange_id INTEGER NOT NULL REFERENCES exchanges(id) ON DELETE CASCADE,
    symbol VARCHAR(20) NOT NULL, -- Global symbol format (e.g., BTC, ETH)
    interval VARCHAR(10) NOT NULL, -- Normalized interval (e.g., 1h, 5m, 1d)
    earliest_timestamp TIMESTAMPTZ NOT NULL, -- Discovered earliest kline timestamp
    discovered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), -- When discovery was performed
    api_calls_used INTEGER NOT NULL DEFAULT 0, -- Number of API calls during discovery
    duration_ms INTEGER NOT NULL DEFAULT 0, -- Discovery duration in milliseconds
    UNIQUE(exchange_id, symbol, interval) -- One cache entry per symbol/interval/exchange
);

-- Index for efficient lookups
CREATE INDEX idx_kline_discovery_cache_lookup ON kline_discovery_cache(exchange_id, symbol, interval);

-- Index for cleanup queries (e.g., delete old cache entries)
CREATE INDEX idx_kline_discovery_cache_discovered_at ON kline_discovery_cache(discovered_at);
