-- Create slippage table for storing trade execution price impact calculations
CREATE TABLE IF NOT EXISTS slippage (
    id BIGSERIAL PRIMARY KEY,
    exchange_id INTEGER NOT NULL REFERENCES exchanges(id),
    symbol VARCHAR(20) NOT NULL,
    ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Mid price reference (average of best bid and best ask)
    mid_price DECIMAL(20, 8) NOT NULL,

    -- Trade amount (USD notional)
    trade_amount DECIMAL(20, 2) NOT NULL,

    -- Buy slippage (executing on asks - price moves up)
    buy_avg_price DECIMAL(20, 8),
    buy_slippage_bps DECIMAL(10, 2),
    buy_slippage_pct DECIMAL(10, 4),
    buy_total_cost DECIMAL(20, 2),
    buy_feasible BOOLEAN NOT NULL,

    -- Sell slippage (executing on bids - price moves down)
    sell_avg_price DECIMAL(20, 8),
    sell_slippage_bps DECIMAL(10, 2),
    sell_slippage_pct DECIMAL(10, 4),
    sell_total_cost DECIMAL(20, 2),
    sell_feasible BOOLEAN NOT NULL,

    CONSTRAINT slippage_unique UNIQUE (exchange_id, symbol, ts, trade_amount)
);

-- Indexes for efficient queries
CREATE INDEX IF NOT EXISTS idx_slippage_exchange_symbol_time
    ON slippage(exchange_id, symbol, ts DESC);

CREATE INDEX IF NOT EXISTS idx_slippage_symbol_time
    ON slippage(symbol, ts DESC);

CREATE INDEX IF NOT EXISTS idx_slippage_trade_amount
    ON slippage(trade_amount);

-- Add comment
COMMENT ON TABLE slippage IS 'Slippage calculations for fixed trade amounts (1K, 10K, 50K, 100K, 500K USD) across exchanges';
