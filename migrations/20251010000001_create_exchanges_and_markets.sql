-- Create exchanges table
CREATE TABLE IF NOT EXISTS exchanges (
  id SERIAL PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  created_at TIMESTAMP WITH TIME ZONE DEFAULT now()
);

-- Create index on exchange name
CREATE INDEX idx_exchanges_name ON exchanges(name);

-- Create markets table
CREATE TABLE IF NOT EXISTS markets (
  id BIGSERIAL PRIMARY KEY,
  exchange_id INT NOT NULL REFERENCES exchanges(id) ON DELETE CASCADE,
  symbol TEXT NOT NULL,
  base_currency TEXT,
  quote_currency TEXT,
  contract_type TEXT,
  tick_size NUMERIC,
  lot_size NUMERIC,
  metadata JSONB,
  created_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
  UNIQUE (exchange_id, symbol)
);

-- Create indexes on markets
CREATE INDEX idx_markets_exchange_id ON markets(exchange_id);
CREATE INDEX idx_markets_symbol ON markets(symbol);
CREATE INDEX idx_markets_exchange_symbol ON markets(exchange_id, symbol);
