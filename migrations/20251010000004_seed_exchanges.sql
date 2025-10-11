-- Seed known exchanges
-- Using ON CONFLICT DO NOTHING to make this idempotent

INSERT INTO exchanges (name) VALUES
  ('binance'),
  ('bybit'),
  ('kucoin'),
  ('lighter'),
  ('paradex'),
  ('hyperliquid'),
  ('cryptocom')
ON CONFLICT (name) DO NOTHING;
