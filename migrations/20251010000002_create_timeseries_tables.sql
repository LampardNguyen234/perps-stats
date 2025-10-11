-- Create tickers table (partitioned by timestamp)
CREATE TABLE IF NOT EXISTS tickers (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  market_id BIGINT REFERENCES markets(id),
  symbol TEXT NOT NULL,
  last_price NUMERIC,
  mark_price NUMERIC,
  index_price NUMERIC,
  best_bid NUMERIC,
  best_ask NUMERIC,
  volume_24h NUMERIC,
  turnover_24h NUMERIC,
  price_change_24h NUMERIC,
  high_24h NUMERIC,
  low_24h NUMERIC,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  received_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create orderbooks table (liquidity snapshots, partitioned by timestamp)
CREATE TABLE IF NOT EXISTS orderbooks (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  market_id BIGINT REFERENCES markets(id),
  symbol TEXT NOT NULL,
  bids_notional NUMERIC,
  asks_notional NUMERIC,
  raw_book JSONB,
  spread_bps INTEGER,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  received_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create trades table (partitioned by timestamp)
CREATE TABLE IF NOT EXISTS trades (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  market_id BIGINT REFERENCES markets(id),
  symbol TEXT NOT NULL,
  trade_id TEXT,
  price NUMERIC NOT NULL,
  size NUMERIC NOT NULL,
  side TEXT,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  received_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
  raw JSONB,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create funding_rates table (partitioned by timestamp)
CREATE TABLE IF NOT EXISTS funding_rates (
  id BIGSERIAL,
  exchange_id INT NOT NULL REFERENCES exchanges(id),
  market_id BIGINT REFERENCES markets(id),
  symbol TEXT NOT NULL,
  rate NUMERIC NOT NULL,
  next_rate NUMERIC,
  ts TIMESTAMP WITH TIME ZONE NOT NULL,
  received_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
  raw JSONB,
  PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);

-- Create ingest_events audit table
CREATE TABLE IF NOT EXISTS ingest_events (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT,
  market_id BIGINT,
  symbol TEXT,
  event_type TEXT,
  status TEXT,
  message TEXT,
  ts TIMESTAMP WITH TIME ZONE DEFAULT now()
);

-- Create index on ingest_events for monitoring
CREATE INDEX idx_ingest_events_ts ON ingest_events(ts DESC);
CREATE INDEX idx_ingest_events_status ON ingest_events(status);
