# Ticker Report Handling in the `start` Command

## Overview

The `start` command has **TWO separate mechanisms** for collecting ticker data:

1. **WebSocket Streaming** (real-time, high-frequency)
2. **Ticker Report Task** (periodic REST API polling, configurable interval)

## How Lighter Handles Ticker Reports

### 1. WebSocket Streaming (Lines 160-592)

**For Lighter:**
- **Enabled**: Yes (line 145-148 in `get_supported_data_types()`)
- **Data Type**: `StreamDataType::Ticker`
- **Storage**: Buffered and stored when batch size is reached (lines 293-312)
- **Frequency**: Real-time (as events arrive from WebSocket)
- **Implementation**: `LighterWsClient` (line 232)

**Process:**
```
Lighter WebSocket → StreamEvent::Ticker → ticker_buffer → batch storage
```

### 2. Ticker Report Task (Lines 798-909)

**For Lighter (and all exchanges):**
- **REST API Call**: `client.get_ticker(&parsed_symbol)` (line 854)
- **Lighter-specific Implementation** (lines 188-199 in `client.rs`):
  ```rust
  async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
      // 1. Fetch orderbook detail (contains ticker data)
      let detail = self.fetch_orderbook_detail(symbol).await?;

      // 2. Fetch orderbook (for best bid/ask quantities)
      let orderbook = self.get_orderbook(symbol, 5).await?;

      // 3. Combine both into a complete Ticker
      conversions::to_ticker_with_orderbook(&detail, &orderbook)
  }
  ```

**Key Points for Lighter:**
- Makes **TWO REST API calls** per symbol:
  1. `/orderBookDetails` endpoint for ticker statistics
  2. `/orderBookOrders?market_id=X&limit=5` for best bid/ask quantities
- Combines the data using `to_ticker_with_orderbook()` conversion
- Returns a complete `Ticker` with accurate best bid/ask prices and quantities

**Process:**
```
Every 30s (default) → REST API fetch → ticker_batch → group by exchange → batch storage
```

## How Other Exchanges Handle Ticker Reports

### Binance, Bybit, Hyperliquid, KuCoin, Paradex

All other exchanges follow the **same pattern** but with simpler implementations:

**REST API Call:**
```rust
client.get_ticker(&parsed_symbol)  // Single API call
```

**Typical Implementation:**
- **Single REST API call** to ticker endpoint (e.g., `/fapi/v1/ticker/24hr` for Binance)
- Returns complete ticker data including best bid/ask in one response
- No need for additional orderbook fetch

## Comparison: Lighter vs Other Exchanges

| Aspect | Lighter | Other Exchanges |
|--------|---------|----------------|
| **WebSocket** | ✅ Yes | ✅ Yes (all supported) |
| **REST API calls** | 2 calls (detail + orderbook) | 1 call (complete ticker) |
| **Best bid/ask source** | Fetched from orderbook | Included in ticker response |
| **Complexity** | Higher (needs combination) | Lower (direct response) |
| **API endpoints** | `/orderBookDetails` + `/orderBookOrders` | `/ticker` or equivalent |

## Data Flow Architecture

```
┌─────────────────────────────────────────────────────┐
│                  start command                       │
├─────────────────────────────────────────────────────┤
│                                                      │
│  Per-Exchange Tasks:                                 │
│  ┌────────────────────────────────────────┐         │
│  │ 1. WebSocket Streaming Task            │         │
│  │    - Real-time ticker events            │         │
│  │    - Batch: 100 events (default)       │         │
│  │    - Storage: Immediate on batch fill  │         │
│  └────────────────────────────────────────┘         │
│                                                      │
│  Global Tasks (all exchanges):                       │
│  ┌────────────────────────────────────────┐         │
│  │ 2. Ticker Report Task (NEW)            │         │
│  │    - Interval: 30s (default)           │         │
│  │    - REST API: get_ticker()            │         │
│  │    - Lighter: 2 API calls per symbol   │         │
│  │    - Others: 1 API call per symbol     │         │
│  │    - Batched storage by exchange       │         │
│  └────────────────────────────────────────┘         │
│                                                      │
│  ┌────────────────────────────────────────┐         │
│  │ 3. Liquidity Report Task               │         │
│  │    - Interval: 30s (default)           │         │
│  │    - Calculates liquidity depth        │         │
│  └────────────────────────────────────────┘         │
└─────────────────────────────────────────────────────┘
```

## Why Both WebSocket AND REST API?

1. **WebSocket**: High-frequency real-time updates (potentially 100+ events/sec)
2. **REST API Report**: Periodic snapshots at controlled intervals (every 30-60s)

**Benefits:**
- **Redundancy**: If WebSocket disconnects, you still have periodic snapshots
- **Different use cases**: WebSocket for real-time monitoring, REST for consistent snapshots
- **Data validation**: Can compare WebSocket vs REST data for accuracy

## Storage Grouping Logic

The ticker report task groups tickers by exchange before storage (lines 870-874 in `src/commands/start.rs`):

```rust
let mut tickers_by_exchange: HashMap<String, Vec<Ticker>> = HashMap::new();
for (i, ticker) in ticker_batch.iter().enumerate() {
    let exchange = &exchanges[i / symbols.len()];
    tickers_by_exchange.entry(exchange.clone()).or_insert_with(Vec::new).push(ticker.clone());
}
```

**Logic**: Assumes tickers are fetched in order:
- Exchange 1: Symbol 1, Symbol 2, Symbol 3
- Exchange 2: Symbol 1, Symbol 2, Symbol 3
- Index calculation: `exchange_index = ticker_index / symbols.len()`

## Code References

### Main Implementation

- **Ticker Report Task**: `src/commands/start.rs:798-909` (`spawn_ticker_report_task()`)
- **WebSocket Streaming**: `src/commands/start.rs:160-592` (`spawn_streaming_task()`)
- **Supported Data Types**: `src/commands/start.rs:118-158` (`get_supported_data_types()`)

### Lighter-Specific Implementation

- **REST API get_ticker()**: `crates/perps-exchanges/src/lighter/client.rs:188-199`
- **Ticker Conversion**: `crates/perps-exchanges/src/lighter/conversions.rs:64-115` (`to_ticker_with_orderbook()`)
- **WebSocket Client**: `crates/perps-exchanges/src/lighter/ws_client.rs`

### Database Storage

- **Store with Exchange**: `crates/perps-database/src/repository.rs:301-341` (`store_tickers_with_exchange()`)
- **Partition Management**: Automatic partition creation before storage

## Summary

**Lighter's ticker report handling is MORE COMPLEX** than other exchanges because:
1. It requires **2 REST API calls** to construct a complete ticker
2. It combines data from `/orderBookDetails` and `/orderBookOrders` endpoints
3. It uses `to_ticker_with_orderbook()` conversion to merge the data

**All exchanges (including Lighter)** get ticker data from BOTH:
- **WebSocket streaming** (real-time, automatic)
- **REST API periodic reports** (controlled interval, manual fetch)

This dual approach provides redundancy and flexibility for different data collection needs.

## Configuration

Default intervals (configurable via CLI):
- `--report-interval`: 30 seconds (controls both liquidity and ticker reports)
- `--batch-size`: 100 events (WebSocket buffer size)

Example:
```bash
cargo run -- start --report-interval 60 --batch-size 50
```

## KuCoin-Specific Limitation

**WebSocket Ticker Stream Does NOT Include 24h Statistics**:
- KuCoin's WebSocket ticker stream (`/contractMarket/ticker:{symbol}`) only provides real-time price data and best bid/ask
- Fields `volume_24h`, `turnover_24h`, `price_change_24h`, `price_change_pct`, `high_price_24h`, `low_price_24h` are **hardcoded to ZERO** in `ws_client.rs:112-117`
- This is intentional - KuCoin's WebSocket API simply doesn't provide these statistics in the ticker stream
- **Solution**: The ticker report task (REST API) provides complete data with all 24h statistics
- **Recommendation**: For KuCoin, rely on REST API ticker reports rather than WebSocket ticker events for accurate 24h data
