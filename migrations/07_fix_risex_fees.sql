-- Fix RISEx taker fee: was stored as 0.0150 (1.5%) instead of 0.000150 (1.5bps)
UPDATE exchanges SET taker_fee = 0.000150 WHERE name = 'risex';
