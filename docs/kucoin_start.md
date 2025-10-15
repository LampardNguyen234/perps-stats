# KuCoin Implementation in the `start` Command

## Overview

KuCoin has a **unique implementation** in the `start` command due to API limitations. Unlike other exchanges, KuCoin does not provide a REST API endpoint for klines, requiring WebSocket streaming instead. Additionally, KuCoin's WebSocket ticker stream lacks 24h statistics.

## Architecture Summary

The `start` command runs **THREE parallel tasks** for KuCoin:

1. **WebSocket Streaming Task** - Real-time data collection (ticker, trade, klines)
2. **Ticker Report Task** - Periodic REST API ticker snapshots with complete 24h statistics
3. **Liquidity Report Task** - Periodic REST API orderbook snapshots for liquidity depth calculation

## Task 1: WebSocket Streaming Task

**Purpose**: Real-time data collection via WebSocket connection

**Implementation**: `src/commands/start.rs:160-592` (`spawn_streaming_task()`)

### Supported Data Types

KuCoin supports the following WebSocket streams (defined at lines 139-144):

```rust
"kucoin" => vec![
    StreamDataType::Ticker,
    StreamDataType::Trade,
    // Note: Orderbook requires incremental update handling (not implemented)
    // Note: Klines added separately based on configuration
],
```

**Important**: When `klines_timeframe` is provided, `StreamDataType::Kline` is automatically added to the data types (lines 196-201).

### WebSocket Topics

| Data Type | Topic Format | Example |
|-----------|-------------|---------|
| Ticker | `/contractMarket/ticker:{symbol}` | `/contractMarket/ticker:XBTUSDTM` |
| Trade | `/contractMarket/execution:{symbol}` | `/contractMarket/execution:XBTUSDTM` |
| Kline | `/contractMarket/limitCandle:{symbol}_{interval}` | `/contractMarket/limitCandle:XBTUSDTM_1hour` |

### Connection Process

1. **Authentication** (`ws_client.rs:30-48`):
   - POST to `https://api-futures.kucoin.com/api/v1/bullet-public`
   - Receives WebSocket token and endpoint
   - Connects to WebSocket URL: `{endpoint}?token={token}`

2. **Symbol Parsing** (lines 176-187):
   - Global format: `BTC` → Exchange format: `XBTUSDTM`
   - No hyphen removal for KuCoin (unlike Binance/Bybit)

3. **Subscription** (lines 449-466):
   - Subscribes to ticker, trade, and klines topics for each symbol
   - Uses `subscribe` message type with unique IDs

### Data Conversion

#### Ticker Conversion (`ws_client.rs:83-120`)

```rust
fn convert_ticker(&self, ws_ticker: &KuCoinWsTicker) -> Result<Ticker> {
    Ok(Ticker {
        symbol: ws_ticker.symbol.clone(),
        last_price: parse_from_option(ws_ticker.last_traded_price),
        mark_price: parse_from_option(ws_ticker.mark_price),
        index_price: parse_from_option(ws_ticker.index_price),
        best_bid_price: parse_from_option(ws_ticker.best_bid_price),
        best_bid_qty: ws_ticker.best_bid_size.map(Decimal::from).unwrap_or(ZERO),
        best_ask_price: parse_from_option(ws_ticker.best_ask_price),
        best_ask_qty: ws_ticker.best_ask_size.map(Decimal::from).unwrap_or(ZERO),
        volume_24h: Decimal::ZERO,        // NOT AVAILABLE in WebSocket
        turnover_24h: Decimal::ZERO,      // NOT AVAILABLE in WebSocket
        price_change_24h: Decimal::ZERO,  // NOT AVAILABLE in WebSocket
        price_change_pct: Decimal::ZERO,  // NOT AVAILABLE in WebSocket
        high_price_24h: Decimal::ZERO,    // NOT AVAILABLE in WebSocket
        low_price_24h: Decimal::ZERO,     // NOT AVAILABLE in WebSocket
        timestamp: Utc.timestamp_millis_opt(ws_ticker.timestamp).unwrap_or_else(Utc::now),
    })
}
```

**Critical Limitation**: Lines 112-117 hardcode 24h statistics to ZERO because KuCoin's WebSocket ticker stream does not provide these fields. This is an **API limitation**, not a bug.

#### Trade Conversion (`ws_client.rs:123-141`)

```rust
fn convert_trade(&self, ws_execution: &KuCoinWsExecution) -> Result<Trade> {
    Ok(Trade {
        id: ws_execution.trade_id.clone(),
        symbol: ws_execution.symbol.clone(),
        price: Decimal::from_str(&ws_execution.price)?,
        quantity: Decimal::from(ws_execution.size),
        side: match ws_execution.side.as_str() {
            "buy" => OrderSide::Buy,
            "sell" => OrderSide::Sell,
            _ => OrderSide::Buy,
        },
        timestamp: {
            // KuCoin timestamps are in nanoseconds
            let seconds = ws_execution.timestamp / 1_000_000_000;
            let nanos = (ws_execution.timestamp % 1_000_000_000) as u32;
            Utc.timestamp_opt(seconds, nanos).unwrap_or_else(Utc::now)
        },
    })
}
```

#### Kline Conversion (`ws_client.rs:165-209`)

KuCoin klines use a **unique format**:

**WebSocket Response**:
```json
{
  "type": "message",
  "topic": "/contractMarket/limitCandle:XBTUSDTM_1hour",
  "data": {
    "symbol": "XBTUSDTM",
    "candles": [
      "1760280000",  // [0] start_time (seconds)
      "115000.5",    // [1] open
      "115500.0",    // [2] close
      "115800.0",    // [3] high
      "114900.0",    // [4] low
      "1234.5",      // [5] volume (base volume)
      "142000000.0"  // [6] amount (quote volume/turnover)
    ],
    "time": 1760280000000
  }
}
```

**Interval Conversion** (`ws_client.rs:146-162`):
- Standard format → KuCoin format
- `1m` → `1min`, `1h` → `1hour`, `1d` → `1day`, `1w` → `1week`
- Applied when subscribing to klines topic

**Normalization** (`ws_client.rs:195`):
- After conversion, interval is normalized back to standard format
- Uses `perps_core::utils::normalize_interval()`
- Ensures database consistency: `1hour` → `1h`, `1min` → `1m`

### Batch Storage

WebSocket events are buffered and stored in batches (lines 293-398):

| Buffer | Batch Size | Database Table | Partition Key |
|--------|-----------|----------------|---------------|
| `ticker_buffer` | 100 (default) | `tickers` | `ts` (timestamp) |
| `trade_buffer` | 100 (default) | `trades` | `ts` (timestamp) |
| `kline_buffer` | 100 (default) | `klines` | `open_time` |

**Storage Process**:
1. Events accumulate in buffer
2. When buffer reaches `batch_size`, trigger storage
3. Ensure partitions exist for timestamp range
4. Store batch with exchange attribution
5. Clear buffer

**Example** (lines 377-398):
```rust
StreamEvent::Kline(kline) => {
    kline_buffer.push(kline);
    if kline_buffer.len() >= batch_size {
        let repo = repository.lock().await;

        // Ensure partitions exist
        if let Some(first_kline) = kline_buffer.first() {
            if let Some(last_kline) = kline_buffer.last() {
                repo.ensure_partitions_for_range(
                    "klines",
                    first_kline.open_time,
                    last_kline.open_time,
                ).await?;
            }
        }

        // Store batch
        repo.store_klines_with_exchange(&exchange, &kline_buffer).await?;
        kline_buffer.clear();
    }
}
```

### Reconnection Logic

KuCoin WebSocket connections are managed with automatic reconnection (lines 204-587):

**Features**:
- Exponential backoff (starts at 5s, max 300s)
- Configurable max attempts (default: unlimited)
- Buffer flushing before reconnection
- Connection attempt logging

**Process**:
1. Initial connection attempt
2. On disconnect/error:
   - Flush all buffers to database
   - Increment reconnect attempt counter
   - Check if max attempts reached
   - Wait with exponential backoff
   - Retry connection
3. On successful reconnection:
   - Reset attempt counter and backoff delay
   - Create fresh buffers
   - Resume streaming

## Task 2: Ticker Report Task

**Purpose**: Periodic REST API ticker snapshots with **COMPLETE 24h statistics**

**Implementation**: `src/commands/start.rs:798-909` (`spawn_ticker_report_task()`)

### Why Separate from WebSocket?

WebSocket tickers have **ZERO values** for 24h statistics. The ticker report task provides:
- Accurate 24h volume
- Accurate 24h turnover
- Accurate 24h price changes
- Accurate 24h high/low prices

### Implementation Details

**Frequency**: Controlled by `--report-interval` (default: 30 seconds)

**Process** (lines 834-863):
1. Wait for interval tick
2. For each exchange and symbol:
   - Parse symbol to exchange format (`BTC` → `XBTUSDTM`)
   - Call `client.get_ticker(&parsed_symbol)`
   - Accumulate in `ticker_batch`
3. Group tickers by exchange
4. Store batch to database with exchange attribution

### KuCoin's `get_ticker()` Implementation

**File**: `crates/perps-exchanges/src/kucoin/client.rs:243-314`

**API Calls** (2 calls per symbol):

```rust
async fn get_ticker(&self, symbol: &str) -> Result<Ticker> {
    // 1. Get detailed contract info for this specific symbol (includes 24h stats)
    let contract: KucoinContractDetail = self
        .get(&format!("/api/v1/contracts/{}", symbol))
        .await?;

    // 2. Get ticker for real-time best bid/ask
    let ticker: KucoinTicker = self
        .get(&format!("/api/v1/ticker?symbol={}", symbol))
        .await?;

    // Combine both responses into complete Ticker
}
```

**Optimization Note**: Before the fix (2025-10-13), this method fetched ALL contracts via `/api/v1/contracts/active`, which was extremely inefficient. Now it fetches only the specific contract.

### Data Sources

| Field | Source | Endpoint |
|-------|--------|----------|
| `last_price` | Contract detail | `/api/v1/contracts/{symbol}` |
| `mark_price` | Contract detail | `/api/v1/contracts/{symbol}` |
| `index_price` | Contract detail | `/api/v1/contracts/{symbol}` |
| `best_bid_price` | Ticker | `/api/v1/ticker?symbol={symbol}` |
| `best_bid_qty` | Ticker | `/api/v1/ticker?symbol={symbol}` |
| `best_ask_price` | Ticker | `/api/v1/ticker?symbol={symbol}` |
| `best_ask_qty` | Ticker | `/api/v1/ticker?symbol={symbol}` |
| `volume_24h` | Contract detail | `/api/v1/contracts/{symbol}` |
| `turnover_24h` | Contract detail | `/api/v1/contracts/{symbol}` |
| `price_change_24h` | Contract detail | `/api/v1/contracts/{symbol}` |
| `price_change_pct` | Contract detail | `/api/v1/contracts/{symbol}` |
| `high_price_24h` | Contract detail | `/api/v1/contracts/{symbol}` |
| `low_price_24h` | Contract detail | `/api/v1/contracts/{symbol}` |

### Fallback Logic

**Price Fallbacks** (`client.rs:252-273`):
```rust
let last_price = contract
    .last_trade_price
    .and_then(Decimal::from_f64)
    .unwrap_or_else(|| {
        tracing::debug!("KuCoin {} last_trade_price is None, using ZERO", symbol);
        Decimal::ZERO
    });

let mark_price = contract
    .mark_price
    .and_then(Decimal::from_f64)
    .unwrap_or_else(|| {
        tracing::debug!("KuCoin {} mark_price is None, using last_price fallback", symbol);
        last_price  // Fallback to last_price instead of ZERO
    });

let index_price = contract
    .index_price
    .and_then(Decimal::from_f64)
    .unwrap_or_else(|| {
        tracing::debug!("KuCoin {} index_price is None, using last_price fallback", symbol);
        last_price  // Fallback to last_price instead of ZERO
    });
```

**24h Statistics Fallbacks** (`client.rs:282-324`):
- All fields default to `Decimal::ZERO` if `None`
- Debug logging indicates which fields are missing
- Useful for diagnosing API data quality issues

### Contract Detail Response Example

```json
{
  "code": "200000",
  "data": {
    "symbol": "XBTUSDTM",
    "lastTradePrice": 115338.0,
    "markPrice": 115321.92,
    "indexPrice": 115321.11,
    "volumeOf24h": 12701.592,
    "turnoverOf24h": 1439942182.512573,
    "highPrice": 115965.3,
    "lowPrice": 109870.0,
    "priceChg": 5423.8,
    "priceChgPct": 0.0493
  }
}
```

### Ticker Response Example

```json
{
  "code": "200000",
  "data": {
    "symbol": "XBTUSDTM",
    "bestBidPrice": "115340.6",
    "bestBidSize": 1760,
    "bestAskPrice": "115340.7",
    "bestAskSize": 38,
    "ts": 1760324003702000000
  }
}
```

## Task 3: Liquidity Report Task

**Purpose**: Periodic orderbook snapshots and liquidity depth calculation

**Implementation**: `src/commands/start.rs:695-796` (`spawn_liquidity_report_task()`)

### Process

**Frequency**: Controlled by `--report-interval` (default: 30 seconds)

**Steps** (lines 732-788):
1. Wait for interval tick
2. For each exchange and symbol:
   - Parse symbol to exchange format
   - Call `common::fetch_and_calculate_liquidity()`
     - Fetches orderbook via REST API
     - Calculates liquidity depth at various bps spreads
   - Store `LiquidityDepthStats` to database

### KuCoin's `get_orderbook()` Implementation

**File**: `crates/perps-exchanges/src/kucoin/client.rs:144-177`

**API Call**: Single REST API call
```rust
async fn get_orderbook(&self, symbol: &str, _depth: u32) -> Result<Orderbook> {
    let response: KucoinOrderbook = self
        .get(&format!("/api/v1/level2/snapshot?symbol={}", symbol))
        .await?;

    // Convert to core Orderbook type
    let bids = response.bids.into_iter()
        .map(|(price, quantity)| OrderbookLevel {
            price: Decimal::from_f64(price).unwrap_or(Decimal::ZERO),
            quantity: Decimal::from(quantity),
        })
        .collect()?;

    let asks = response.asks.into_iter()
        .map(|(price, quantity)| OrderbookLevel {
            price: Decimal::from_f64(price).unwrap_or(Decimal::ZERO),
            quantity: Decimal::from(quantity),
        })
        .collect()?;

    Ok(Orderbook {
        symbol: response.symbol,
        bids,
        asks,
        timestamp: Utc.timestamp_nanos(response.timestamp),
    })
}
```

**Note**: `_depth` parameter is ignored. KuCoin's API returns the full orderbook snapshot.

### Liquidity Depth Calculation

**Implementation**: `crates/perps-aggregator/src/aggregator.rs:135-249` (`calculate_liquidity_depth()`)

**Process**:
1. Calculate mid price from best bid/ask
2. For each bps level (1, 2.5, 5, 10, 20):
   - Calculate price threshold: `mid_price * (1 + bps/10000)`
   - Sum notional value of orders within threshold
   - Store cumulative bid/ask notional
3. Return `LiquidityDepthStats`

**Example Result**:
```rust
LiquidityDepthStats {
    symbol: "BTC".to_string(),
    exchange: "kucoin".to_string(),
    bid_1bps: Decimal::from_str("50000.0").unwrap(),   // $50K liquidity within 1bps
    ask_1bps: Decimal::from_str("48000.0").unwrap(),
    bid_2_5bps: Decimal::from_str("120000.0").unwrap(),
    ask_2_5bps: Decimal::from_str("115000.0").unwrap(),
    // ... more bps levels
    timestamp: Utc::now(),
}
```

## Klines: WebSocket vs REST API

### Why KuCoin Uses WebSocket for Klines

**REST API Limitation**: KuCoin Futures does **NOT** provide a REST API endpoint for klines.

**Implementation** (`client.rs:225-236`):
```rust
async fn get_klines(
    &self,
    _symbol: &str,
    _interval: &str,
    _start_time: Option<chrono::DateTime<chrono::Utc>>,
    _end_time: Option<chrono::DateTime<chrono::Utc>>,
    _limit: Option<u32>,
) -> Result<Vec<Kline>> {
    Err(anyhow!("get_klines is not available from KuCoin Futures REST API. Use WebSocket streaming instead."))
}
```

### Automatic Detection in `start` Command

**Logic** (`start.rs:272-289`):

```rust
// Spawn klines fetching tasks (one per exchange, except KuCoin which uses WebSocket)
for (exchange, symbols) in &exchange_symbols {
    // Skip KuCoin - it uses WebSocket for klines instead
    if exchange == "kucoin" {
        tracing::info!("Skipping REST API klines task for KuCoin (using WebSocket instead)");
        continue;
    }

    let task = spawn_klines_task(
        exchange.clone(),
        symbols.clone(),
        args.klines_interval,
        args.klines_timeframe.clone(),
        repository.clone(),
        shutdown.clone(),
    ).await?;
    tasks.push(task);
}
```

**Result**: KuCoin automatically uses WebSocket for klines, while other exchanges use REST API with periodic fetching.

### Comparison: KuCoin vs Other Exchanges

| Aspect | KuCoin | Binance / Bybit / Hyperliquid / Lighter / Paradex |
|--------|--------|---------------------------------------------------|
| **Klines Source** | WebSocket streaming | REST API periodic fetching |
| **Klines Task** | Included in WebSocket streaming task | Separate `spawn_klines_task()` |
| **Interval Control** | `--klines-timeframe` (e.g., `1h`) | `--klines-interval` (seconds) + `--klines-timeframe` |
| **Data Frequency** | Real-time (as events arrive) | Periodic (every N seconds) |
| **API Endpoint** | None (WebSocket only) | `/api/v1/klines` or equivalent |

## Data Flow Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                    start command (KuCoin)                        │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │ Task 1: WebSocket Streaming                                │ │
│  │ ────────────────────────────                               │ │
│  │                                                            │ │
│  │  Connection:                                               │ │
│  │    POST /api/v1/bullet-public → token + endpoint          │ │
│  │    Connect: wss://...?token=xxx                            │ │
│  │                                                            │ │
│  │  Subscriptions:                                            │ │
│  │    /contractMarket/ticker:XBTUSDTM                         │ │
│  │    /contractMarket/execution:XBTUSDTM                      │ │
│  │    /contractMarket/limitCandle:XBTUSDTM_1hour              │ │
│  │                                                            │ │
│  │  Data Flow:                                                │ │
│  │    Ticker event → ticker_buffer (batch: 100)               │ │
│  │                → store_tickers_with_exchange()             │ │
│  │                → tickers table (with ZERO 24h stats!)      │ │
│  │                                                            │ │
│  │    Trade event → trade_buffer (batch: 100)                 │ │
│  │               → store_trades_with_exchange()               │ │
│  │               → trades table                               │ │
│  │                                                            │ │
│  │    Kline event → kline_buffer (batch: 100)                 │ │
│  │               → store_klines_with_exchange()               │ │
│  │               → klines table                               │ │
│  │                                                            │ │
│  │  Reconnection: Automatic with exponential backoff          │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │ Task 2: Ticker Report (every 30s)                          │ │
│  │ ──────────────────────────────                             │ │
│  │                                                            │ │
│  │  API Calls (per symbol):                                   │ │
│  │    GET /api/v1/contracts/{symbol}                          │ │
│  │      → last_price, mark_price, index_price                 │ │
│  │      → volume_24h, turnover_24h                            │ │
│  │      → price_change_24h, price_change_pct                  │ │
│  │      → high_price_24h, low_price_24h                       │ │
│  │                                                            │ │
│  │    GET /api/v1/ticker?symbol={symbol}                      │ │
│  │      → best_bid_price, best_bid_size                       │ │
│  │      → best_ask_price, best_ask_size                       │ │
│  │                                                            │ │
│  │  Data Flow:                                                │ │
│  │    Combine both responses → complete Ticker                │ │
│  │    → ticker_batch                                          │ │
│  │    → store_tickers_with_exchange()                         │ │
│  │    → tickers table (with COMPLETE 24h stats!)              │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │ Task 3: Liquidity Report (every 30s)                       │ │
│  │ ───────────────────────────────                            │ │
│  │                                                            │ │
│  │  API Call (per symbol):                                    │ │
│  │    GET /api/v1/level2/snapshot?symbol={symbol}             │ │
│  │      → bids: [(price, quantity), ...]                      │ │
│  │      → asks: [(price, quantity), ...]                      │ │
│  │                                                            │ │
│  │  Calculation:                                              │ │
│  │    Orderbook → calculate_liquidity_depth()                 │ │
│  │             → LiquidityDepthStats                          │ │
│  │               (cumulative notional at 1, 2.5, 5, 10, 20bps)│ │
│  │                                                            │ │
│  │  Data Flow:                                                │ │
│  │    LiquidityDepthStats → store_liquidity_depth()           │ │
│  │                        → liquidity_depth table             │ │
│  └────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

## Database Schema

### Tables Affected

| Table | Partition Key | Data Source | Exchange Field |
|-------|--------------|-------------|----------------|
| `tickers` | `ts` (timestamp) | WebSocket stream (incomplete) + REST API report (complete) | `exchange_id` |
| `trades` | `ts` (timestamp) | WebSocket stream | `exchange_id` |
| `klines` | `open_time` | WebSocket stream (KuCoin only) | `exchange_id` |
| `liquidity_depth` | `timestamp` | REST API orderbook snapshot | `exchange` (string) |

### Symbol Normalization

All symbols are automatically normalized before storage:
- Exchange format: `XBTUSDTM` (KuCoin)
- Database storage: `BTC` (normalized)
- Normalization: `perps_core::utils::extract_base_symbol()`

Example:
```rust
// Before storage
symbol: "XBTUSDTM"

// After normalization (in database)
symbol: "BTC"
```

### Ticker Data Quality

**Challenge**: WebSocket tickers have ZERO 24h statistics, REST API tickers have complete data.

**Database Result**:
```sql
SELECT symbol, last_price, volume_24h, ts
FROM tickers
WHERE exchange_id = (SELECT id FROM exchanges WHERE name = 'kucoin')
ORDER BY ts DESC
LIMIT 5;

-- Result:
--  symbol | last_price | volume_24h |              ts
-- --------+------------+------------+-------------------------------
--  BTC    |     115284 |  12687.938 | 2025-10-13 10:01:53.695+07  (REST API)
--  BTC    |          0 |          0 | 2025-10-13 10:01:53.580+07  (WebSocket)
--  BTC    |          0 |          0 | 2025-10-13 10:01:53.546+07  (WebSocket)
```

**Recommendation**: For analysis requiring 24h statistics, filter for non-zero `volume_24h`:

```sql
SELECT * FROM tickers
WHERE exchange_id = (SELECT id FROM exchanges WHERE name = 'kucoin')
  AND volume_24h > 0  -- Only REST API tickers
ORDER BY ts DESC;
```

## Configuration

### CLI Parameters

| Parameter | Default | Description | KuCoin Usage |
|-----------|---------|-------------|--------------|
| `--exchanges` | All exchanges | Comma-separated exchange list | Include `kucoin` |
| `--symbols-file` | `symbols.txt` | Path to symbols file | Global symbols (e.g., `BTC`) |
| `--batch-size` | 100 | WebSocket batch size | Ticker, trade, kline buffers |
| `--klines-interval` | 60 | Klines fetch interval (seconds) | **Not used** (WebSocket instead) |
| `--klines-timeframe` | `1h` | Klines interval/timeframe | Used for WebSocket subscription |
| `--report-interval` | 30 | Report generation interval (seconds) | Ticker + liquidity reports |
| `--max-reconnect-attempts` | 0 (unlimited) | Max WebSocket reconnection attempts | Applied to KuCoin WebSocket |
| `--reconnect-delay-seconds` | 5 | Initial reconnection delay | Exponential backoff start |
| `--max-reconnect-delay-seconds` | 300 | Maximum reconnection delay | Exponential backoff cap |

### Example Commands

**Single Exchange (KuCoin only)**:
```bash
cargo run -- start --exchanges kucoin \
  --symbols-file symbols.txt \
  --batch-size 50 \
  --klines-timeframe 1h \
  --report-interval 30
```

**Multi-Exchange (including KuCoin)**:
```bash
cargo run -- start --exchanges binance,kucoin,hyperliquid \
  --symbols-file symbols.txt \
  --report-interval 60
```

**All Exchanges (KuCoin included automatically)**:
```bash
cargo run -- start \
  --symbols-file symbols.txt
```

## Key Differences from Other Exchanges

| Feature | KuCoin | Other Exchanges |
|---------|--------|-----------------|
| **Klines Source** | WebSocket only | REST API only |
| **Klines Task** | Skipped (uses WebSocket task) | Spawned separately |
| **Ticker Report API Calls** | 2 (contract + ticker) | 1 (single ticker endpoint) |
| **WebSocket Ticker 24h Stats** | ZERO (not available) | Varies (some have, some don't) |
| **Orderbook WebSocket** | Not implemented | Most exchanges support |
| **Symbol Format** | `XBTUSDTM` (BTC special case) | Various formats |
| **WebSocket Auth** | Token-based (requires REST call) | Most are public |

## Known Limitations

1. **WebSocket Ticker Incomplete**:
   - 24h statistics are ZERO
   - Rely on ticker report task for complete data
   - Database contains mixed data quality

2. **Orderbook WebSocket Not Implemented**:
   - Requires incremental update handling
   - Currently relies on REST API snapshots
   - Lower frequency than real-time WebSocket

3. **Token Expiration**:
   - WebSocket tokens may expire
   - Requires reconnection with new token
   - Handled automatically by reconnection logic

4. **Klines Frequency**:
   - Real-time WebSocket means higher frequency
   - More database writes compared to periodic REST API
   - Consider adjusting `--batch-size` for optimization

## Performance Considerations

### WebSocket Batch Size

**Default**: 100 events

**Recommendations**:
- **High-frequency symbols**: Increase to 200-500 to reduce database writes
- **Low-frequency symbols**: Keep at 100 or decrease to 50 for faster storage
- **Multiple symbols**: Scale with number of symbols (e.g., 10 symbols × 50 = 500)

### Report Interval

**Default**: 30 seconds

**Trade-offs**:
- **Shorter interval** (10-15s): More frequent snapshots, higher API load
- **Longer interval** (60-120s): Less API load, lower data granularity

### Database Partitioning

**Automatic**: Daily partitions created automatically

**Maintenance**:
- Enable `--enable-partition-cleanup` to drop old partitions
- Set `--retention-days` to control data retention (default: 30 days)
- Runs cleanup every `--cleanup-interval` seconds (default: 3600s / 1 hour)

## Troubleshooting

### Issue: WebSocket tickers have zero values

**Symptom**: Database shows `volume_24h = 0`, `turnover_24h = 0`

**Cause**: WebSocket ticker stream doesn't provide 24h statistics (API limitation)

**Solution**: This is expected. Use ticker report data (filter by `volume_24h > 0`)

### Issue: No klines being stored

**Symptom**: `klines` table empty for KuCoin

**Cause**:
1. WebSocket klines not enabled (missing `--klines-timeframe`)
2. WebSocket connection failed
3. Batch size not reached

**Solution**:
1. Ensure `--klines-timeframe` is provided
2. Check logs for WebSocket connection status
3. Reduce `--batch-size` for testing

### Issue: Frequent reconnections

**Symptom**: Logs show "Reconnecting to kucoin WebSocket (attempt X)"

**Cause**:
1. Network instability
2. Token expiration
3. API rate limits

**Solution**:
1. Check network connection
2. Increase `--reconnect-delay-seconds` to reduce retry frequency
3. Check KuCoin API status

### Issue: High API rate limits

**Symptom**: HTTP 429 errors in logs

**Cause**: Ticker report + liquidity report calling API too frequently

**Solution**:
1. Increase `--report-interval` (e.g., from 30s to 60s)
2. Reduce number of symbols
3. Monitor KuCoin rate limit headers

## Code References

### Main Files

| File | Lines | Description |
|------|-------|-------------|
| `src/commands/start.rs` | 160-592 | WebSocket streaming task |
| `src/commands/start.rs` | 798-909 | Ticker report task |
| `src/commands/start.rs` | 695-796 | Liquidity report task |
| `src/commands/start.rs` | 272-289 | KuCoin klines task skipping logic |
| `crates/perps-exchanges/src/kucoin/ws_client.rs` | 1-527 | KuCoin WebSocket client implementation |
| `crates/perps-exchanges/src/kucoin/ws_client.rs` | 83-120 | Ticker conversion (ZERO 24h stats) |
| `crates/perps-exchanges/src/kucoin/ws_client.rs` | 165-209 | Kline conversion |
| `crates/perps-exchanges/src/kucoin/client.rs` | 243-314 | REST API `get_ticker()` implementation |
| `crates/perps-exchanges/src/kucoin/client.rs` | 144-177 | REST API `get_orderbook()` implementation |
| `crates/perps-exchanges/src/kucoin/types.rs` | 36-55 | `KucoinTicker` struct (WebSocket ticker) |
| `crates/perps-exchanges/src/kucoin/types.rs` | 110-141 | `KucoinContractDetail` struct (REST contract) |

## Summary

KuCoin's implementation in the `start` command is **unique** due to:

1. **WebSocket klines** instead of REST API (API limitation)
2. **Incomplete WebSocket tickers** (24h stats are ZERO)
3. **Dual ticker sources** (WebSocket for real-time + REST for complete data)
4. **Two REST API calls** for complete ticker data (contract + ticker endpoints)
5. **Automatic task skipping** (klines REST task skipped in favor of WebSocket)

The architecture ensures **data completeness** through:
- REST API ticker reports for accurate 24h statistics
- WebSocket streaming for real-time price and trade data
- REST API orderbook snapshots for liquidity depth calculation

Users should be aware of the **data quality difference** in the `tickers` table and filter accordingly for analysis requiring complete 24h statistics.
