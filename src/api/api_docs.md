# Perps Stats REST API Documentation

**Version:** 0.1.0
**License:** MIT

## Overview

The Perps Stats REST API provides access to perpetual futures market data from multiple cryptocurrency exchanges. This API aggregates real-time and historical data including ticker information, orderbook snapshots, trade history, funding rates, OHLCV candlestick data, and liquidity depth statistics.

### Data Sources

- Binance
- Bybit
- Hyperliquid
- KuCoin
- Paradex
- Extended
- Lighter
- Aster
- Pacifica
- Nado

### Features

- **Real-time & Historical Data**: Access current and historical market data
- **Multi-Exchange Support**: Query data from multiple exchanges simultaneously
- **Time-Series Optimization**: PostgreSQL with TimescaleDB for efficient time-series queries
- **Liquidity Analysis**: Detailed liquidity depth statistics at various spread levels
- **Flexible Querying**: Filter by exchange, symbol, time range, and more

## Base URLs

- **Local Development**: `http://localhost:8080/api/v1`
- **Local IP**: `http://127.0.0.1:8080/api/v1`

## Authentication

**None** - This API is intended for internal/private use or deployment behind an authentication proxy.

---

## Table of Contents

- [Health & Monitoring](#health--monitoring)
  - [Health Check](#health-check)
  - [Database Statistics](#database-statistics)
- [Exchange & Symbol Information](#exchange--symbol-information)
  - [List Exchanges](#list-exchanges)
  - [List Symbols](#list-symbols)
- [Ticker Data](#ticker-data)
  - [Get Latest Tickers](#get-latest-tickers)
  - [Get Ticker History](#get-ticker-history)
- [Liquidity Depth](#liquidity-depth)
  - [Get Latest Liquidity](#get-latest-liquidity)
  - [Get Liquidity History](#get-liquidity-history)
- [Orderbooks](#orderbooks)
  - [Get Latest Orderbook](#get-latest-orderbook)
  - [Get Orderbook History](#get-orderbook-history)
- [Slippage](#slippage)
  - [Get Latest Slippage](#get-latest-slippage)
  - [Get Slippage History](#get-slippage-history)
- [Trades](#trades) *(Disabled)*
- [Funding Rates](#funding-rates) *(Disabled)*
- [Klines (OHLCV)](#klines-ohlcv) *(Disabled)*
- [Common Parameters](#common-parameters)
- [Error Handling](#error-handling)
- [Data Types](#data-types)

---

## Health & Monitoring

### Health Check

Check if the API server and database are operational.

**Endpoint:** `GET /health`

**Response:**

```json
{
  "status": "ok",
  "database": "healthy",
  "timestamp": "2025-12-10T13:26:16.864618Z",
  "version": "0.1.0"
}
```

**Response Fields:**

| Field | Type | Description | Values |
|-------|------|-------------|--------|
| `status` | string | Overall server status | `ok`, `degraded` |
| `database` | string | Database connection status | `healthy`, `unhealthy` |
| `timestamp` | string | Current server time (ISO 8601) | - |
| `version` | string | API version | - |

**Example Request:**

```bash
curl http://localhost:8080/api/v1/health
```

---

### Database Statistics

Get aggregated statistics about data in the database.

**Endpoint:** `GET /stats`

**Response:**

```json
{
  "total_tickers": 15420,
  "total_liquidity_depth": 8934,
  "total_orderbooks": 12456,
  "total_trades": 98234,
  "total_klines": 45123,
  "total_funding_rates": 2341,
  "oldest_data": 1733788800,
  "newest_data": 1765431600
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `total_tickers` | integer | Total number of ticker records |
| `total_liquidity_depth` | integer | Total number of liquidity depth records |
| `total_orderbooks` | integer | Total number of orderbook snapshots |
| `total_trades` | integer | Total number of trade records |
| `total_klines` | integer | Total number of kline records |
| `total_funding_rates` | integer | Total number of funding rate records |
| `oldest_data` | integer | Unix timestamp of oldest data record (nullable) |
| `newest_data` | integer | Unix timestamp of newest data record (nullable) |

**Example Request:**

```bash
curl http://localhost:8080/api/v1/stats
```

---

## Exchange & Symbol Information

### List Exchanges

Get metadata for all supported exchanges including maker/taker fees.

**Endpoint:** `GET /exchanges`

**Response:**

```json
[
  {
    "id": 1,
    "name": "binance",
    "maker_fee": 0.0,
    "taker_fee": 0.000084
  },
  {
    "id": 2,
    "name": "bybit",
    "maker_fee": 0.0,
    "taker_fee": 0.0003
  },
  {
    "id": 10,
    "name": "nado",
    "maker_fee": -0.00008,
    "taker_fee": 0.00015
  }
]
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | integer | Unique exchange ID |
| `name` | string | Exchange name |
| `maker_fee` | number | Maker fee (negative = rebate) |
| `taker_fee` | number | Taker fee |

**Example Request:**

```bash
curl http://localhost:8080/api/v1/exchanges
```

---

### List Symbols

Get a list of all unique symbols available in the database, sorted alphabetically.

**Endpoint:** `GET /symbols`

**Response:**

```json
{
  "data": {
    "symbols": ["BTC", "ETH", "SOL", "HYPE", "DOGE"],
    "count": 5
  },
  "timestamp": 1765431600
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `data.symbols` | array[string] | List of unique symbols |
| `data.count` | integer | Total number of symbols |
| `timestamp` | integer | Response timestamp (Unix seconds) |

**Example Request:**

```bash
curl http://localhost:8080/api/v1/symbols
```

---

## Ticker Data

### Get Latest Tickers

Get the most recent ticker data for a symbol.

**Endpoint:** `GET /tickers`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `exchange` | string | No | Exchange name filter |

**Behavior:**
- **With exchange**: Returns ticker for that specific exchange/symbol combination (single result)
- **Without exchange**: Returns latest ticker from each exchange for the symbol (multiple results)

**Response:**

```json
{
  "data": [
    {
      "exchange": "binance",
      "symbol": "BTC",
      "last_price": "98234.50",
      "mark_price": "98235.00",
      "index_price": "98233.00",
      "best_bid_price": "98230.00",
      "best_bid_qty": "1.5",
      "best_ask_price": "98235.00",
      "best_ask_qty": "2.3",
      "volume_24h": "1234567.89",
      "turnover_24h": "121234567.89",
      "open_interest": "45678.90",
      "open_interest_notional": "4487654321.00",
      "price_change_24h": "750.50",
      "price_change_pct": "0.0077",
      "high_price_24h": "99000.00",
      "low_price_24h": "97500.00",
      "timestamp": "2025-12-10T13:30:00Z"
    }
  ],
  "timestamp": "2025-12-10T13:30:05Z"
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `exchange` | string | Exchange name |
| `symbol` | string | Trading symbol |
| `last_price` | string | Last traded price |
| `mark_price` | string | Mark price |
| `index_price` | string | Index price |
| `best_bid_price` | string | Best bid price |
| `best_bid_qty` | string | Best bid quantity |
| `best_ask_price` | string | Best ask price |
| `best_ask_qty` | string | Best ask quantity |
| `volume_24h` | string | 24-hour trading volume |
| `turnover_24h` | string | 24-hour turnover |
| `open_interest` | string | Open interest in contracts |
| `open_interest_notional` | string | Open interest notional value |
| `price_change_24h` | string | 24-hour price change (absolute) |
| `price_change_pct` | string | 24-hour price change (percentage as decimal) |
| `high_price_24h` | string | 24-hour high price |
| `low_price_24h` | string | 24-hour low price |
| `timestamp` | string | Data timestamp (ISO 8601) |

**Example Requests:**

```bash
# Get BTC ticker from all exchanges
curl "http://localhost:8080/api/v1/tickers?symbol=BTC"

# Get BTC ticker from Binance only
curl "http://localhost:8080/api/v1/tickers?symbol=BTC&exchange=binance"
```

---

### Get Ticker History

Get historical ticker data within a time range for a symbol.

**Endpoint:** `GET /tickers/history`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `exchange` | string | No | Exchange name filter |
| `start` | integer | No | Start time (Unix timestamp in seconds, defaults to 24h ago) |
| `end` | integer | No | End time (Unix timestamp in seconds, defaults to now) |
| `limit` | integer | No | Max records to return (1-500, default 100) |
| `offset` | integer | No | Number of records to skip (default 0) |

**Behavior:**
- **With exchange**: Returns tickers for that specific exchange/symbol combination
- **Without exchange**: Returns tickers for that symbol across all exchanges

**Response:**

```json
{
  "pagination": {
    "total": null,
    "limit": 100,
    "offset": 0,
    "count": 50
  },
  "data": [
    {
      "exchange": "binance",
      "symbol": "BTC",
      "last_price": "98234.50",
      "mark_price": "98235.00",
      "timestamp": "2025-12-10T13:30:00Z"
    },
    {
      "exchange": "binance",
      "symbol": "BTC",
      "last_price": "98200.00",
      "mark_price": "98201.00",
      "timestamp": "2025-12-10T13:29:00Z"
    }
  ]
}
```

**Example Requests:**

```bash
# Get last 100 BTC tickers from all exchanges
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC"

# Get BTC tickers from Binance in specific time range
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&exchange=binance&start=1733788800&end=1733875200"

# Get with pagination
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&limit=50&offset=100"
```

---

## Liquidity Depth

### Get Latest Liquidity

Get the most recent liquidity depth statistics for a symbol.

**Endpoint:** `GET /liquidity`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `exchange` | string | No | Exchange name filter |

**Behavior:**
- **With exchange**: Returns liquidity for that specific exchange/symbol combination (single result)
- **Without exchange**: Returns latest liquidity from each exchange for the symbol (multiple results)

**Response:**

```json
{
  "data": [
    {
      "exchange": "binance",
      "symbol": "BTC",
      "bid_1bps": "50000.00",
      "bid_2_5bps": "120000.00",
      "bid_5bps": "250000.00",
      "bid_10bps": "500000.00",
      "bid_20bps": "1000000.00",
      "ask_1bps": "48000.00",
      "ask_2_5bps": "115000.00",
      "ask_5bps": "240000.00",
      "ask_10bps": "480000.00",
      "ask_20bps": "950000.00",
      "timestamp": "2025-12-10T13:30:00Z"
    }
  ],
  "timestamp": "2025-12-10T13:30:05Z"
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `exchange` | string | Exchange name |
| `symbol` | string | Trading symbol |
| `bid_1bps` | string | Cumulative bid liquidity at 1 basis point spread |
| `bid_2_5bps` | string | Cumulative bid liquidity at 2.5 basis points spread |
| `bid_5bps` | string | Cumulative bid liquidity at 5 basis points spread |
| `bid_10bps` | string | Cumulative bid liquidity at 10 basis points spread |
| `bid_20bps` | string | Cumulative bid liquidity at 20 basis points spread |
| `ask_1bps` | string | Cumulative ask liquidity at 1 basis point spread |
| `ask_2_5bps` | string | Cumulative ask liquidity at 2.5 basis points spread |
| `ask_5bps` | string | Cumulative ask liquidity at 5 basis points spread |
| `ask_10bps` | string | Cumulative ask liquidity at 10 basis points spread |
| `ask_20bps` | string | Cumulative ask liquidity at 20 basis points spread |
| `timestamp` | string | Data timestamp (ISO 8601) |

**Example Requests:**

```bash
# Get BTC liquidity from all exchanges
curl "http://localhost:8080/api/v1/liquidity?symbol=BTC"

# Get BTC liquidity from Hyperliquid only
curl "http://localhost:8080/api/v1/liquidity?symbol=BTC&exchange=hyperliquid"
```

---

### Get Liquidity History

Get historical liquidity depth data within a time range for a symbol.

**Endpoint:** `GET /liquidity/history`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `exchange` | string | No | Exchange name filter |
| `start` | integer | No | Start time (Unix timestamp in seconds) |
| `end` | integer | No | End time (Unix timestamp in seconds) |
| `limit` | integer | No | Max records to return (1-500, default 100) |
| `offset` | integer | No | Number of records to skip (default 0) |

**Behavior:**
- **With exchange**: Returns liquidity for that specific exchange/symbol combination
- **Without exchange**: Returns liquidity for that symbol across all exchanges

**Response:** Same structure as paginated ticker history, with liquidity depth data.

**Example Requests:**

```bash
# Get last 100 BTC liquidity snapshots from all exchanges
curl "http://localhost:8080/api/v1/liquidity/history?symbol=BTC"

# Get BTC liquidity from Binance in specific time range
curl "http://localhost:8080/api/v1/liquidity/history?symbol=BTC&exchange=binance&start=1733788800&end=1733875200"
```

---

## Orderbooks

### Get Latest Orderbook

Get the most recent orderbook snapshot for a symbol.

**Endpoint:** `GET /orderbooks`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `exchange` | string | No | Exchange name filter |

**Behavior:**
- **With exchange**: Returns orderbook for that specific exchange/symbol combination (single result)
- **Without exchange**: Returns latest orderbook from each exchange for the symbol (multiple results)

**Response:**

```json
{
  "data": [
    {
      "exchange": "binance",
      "symbol": "BTC",
      "bids": [
        ["98230.00", "1.5"],
        ["98225.00", "2.3"],
        ["98220.00", "0.8"]
      ],
      "asks": [
        ["98235.00", "1.2"],
        ["98240.00", "3.1"],
        ["98245.00", "0.5"]
      ],
      "timestamp": "2025-12-10T13:30:00Z"
    }
  ],
  "timestamp": "2025-12-10T13:30:05Z"
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `exchange` | string | Exchange name |
| `symbol` | string | Trading symbol |
| `bids` | array | Bid orders (buy side) as `[price, quantity]` pairs |
| `asks` | array | Ask orders (sell side) as `[price, quantity]` pairs |
| `timestamp` | string | Data timestamp (ISO 8601) |

**Example Requests:**

```bash
# Get BTC orderbook from all exchanges
curl "http://localhost:8080/api/v1/orderbooks?symbol=BTC"

# Get BTC orderbook from Bybit only
curl "http://localhost:8080/api/v1/orderbooks?symbol=BTC&exchange=bybit"
```

---

### Get Orderbook History

Get historical orderbook snapshots within a time range for a symbol.

**Endpoint:** `GET /orderbooks/history`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `exchange` | string | No | Exchange name filter |
| `start` | integer | No | Start time (Unix timestamp in seconds) |
| `end` | integer | No | End time (Unix timestamp in seconds) |
| `limit` | integer | No | Max records to return (1-500, default 100) |
| `offset` | integer | No | Number of records to skip (default 0) |

**Behavior:**
- **With exchange**: Returns orderbooks for that specific exchange/symbol combination
- **Without exchange**: Returns orderbooks for that symbol across all exchanges

**Response:** Same structure as paginated response, with orderbook data.

**Example Requests:**

```bash
# Get last 100 BTC orderbook snapshots
curl "http://localhost:8080/api/v1/orderbooks/history?symbol=BTC"

# Get BTC orderbooks from Hyperliquid in specific time range
curl "http://localhost:8080/api/v1/orderbooks/history?symbol=BTC&exchange=hyperliquid&start=1733788800&limit=50"
```

---

## Slippage

### Get Latest Slippage

Get the most recent slippage calculations for a symbol and trade amount.

**Endpoint:** `GET /slippage`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `amount` | string | **Yes** | Trade amount in USD (e.g., "10000") |
| `exchange` | string | No | Exchange name filter |

**Behavior:**
- **With exchange**: Returns slippage for that specific exchange/symbol/amount combination
- **Without exchange**: Returns latest slippage from each exchange for the symbol/amount

**Response:**

```json
{
  "data": [
    {
      "exchange": "binance",
      "symbol": "BTC",
      "trade_amount": "10000.00",
      "mid_price": "98232.50",
      "buy_avg_price": "98240.00",
      "buy_slippage_bps": "7.62",
      "buy_feasible": true,
      "sell_avg_price": "98225.00",
      "sell_slippage_bps": "7.62",
      "sell_feasible": true,
      "timestamp": "2025-12-10T13:30:00Z"
    }
  ],
  "timestamp": 1733875205
}
```

**Response Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `exchange` | string | Exchange name |
| `symbol` | string | Trading symbol |
| `trade_amount` | string | Trade amount in USD |
| `mid_price` | string | Mid-price at time of calculation |
| `buy_avg_price` | string | Average execution price for buy |
| `buy_slippage_bps` | string | Buy slippage in basis points |
| `buy_feasible` | boolean | Whether buy trade is feasible |
| `sell_avg_price` | string | Average execution price for sell |
| `sell_slippage_bps` | string | Sell slippage in basis points |
| `sell_feasible` | boolean | Whether sell trade is feasible |
| `timestamp` | string | Data timestamp (ISO 8601) |

**Example Requests:**

```bash
# Get BTC slippage for $10,000 trade from all exchanges
curl "http://localhost:8080/api/v1/slippage?symbol=BTC&amount=10000"

# Get BTC slippage for $50,000 trade from Binance
curl "http://localhost:8080/api/v1/slippage?symbol=BTC&amount=50000&exchange=binance"
```

---

### Get Slippage History

Get historical slippage calculations within a time range.

**Endpoint:** `GET /slippage/history`

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `symbol` | string | **Yes** | Trading symbol (e.g., BTC, ETH, SOL) |
| `amount` | string | **Yes** | Trade amount in USD (e.g., "10000") |
| `exchange` | string | No | Exchange name filter |
| `start` | integer | No | Start time (Unix timestamp in seconds) |
| `end` | integer | No | End time (Unix timestamp in seconds) |
| `limit` | integer | No | Max records to return (1-500, default 100) |
| `offset` | integer | No | Number of records to skip (default 0) |

**Behavior:**
- **With exchange**: Returns slippage for that specific exchange/symbol/amount combination
- **Without exchange**: Returns slippage for that symbol/amount across all exchanges

**Response:** Same structure as paginated response, with slippage data.

**Example Requests:**

```bash
# Get last 100 BTC slippage calculations for $10k trades
curl "http://localhost:8080/api/v1/slippage/history?symbol=BTC&amount=10000"

# Get BTC slippage history from Binance in time range
curl "http://localhost:8080/api/v1/slippage/history?symbol=BTC&amount=10000&exchange=binance&start=1733788800&end=1733875200"
```

---

## Trades

**Status:** Currently disabled (no data available yet)

Get recent trade history for a specific exchange and symbol.

**Endpoint:** `GET /trades` *(disabled)*

---

## Funding Rates

**Status:** Currently disabled (no data available yet)

Get funding rate data for perpetual futures contracts.

**Endpoints:**
- `GET /funding-rates` - Get latest funding rate *(disabled)*
- `GET /funding-rates/history` - Get funding rate history *(disabled)*

---

## Klines (OHLCV)

**Status:** Currently disabled (no data available yet)

Get OHLCV candlestick data for charting.

**Endpoint:** `GET /klines` *(disabled)*

**Parameters (when enabled):**
- `exchange` (required) - Exchange name
- `symbol` (required) - Trading symbol
- `interval` (required) - Candlestick interval (1m, 5m, 15m, 1h, 4h, 1d, etc.)
- `start` - Start time
- `end` - End time
- `limit` - Max records
- `offset` - Pagination offset

---

## Common Parameters

### Exchange Parameter

**Name:** `exchange`
**Type:** string
**Required:** No
**Description:** Exchange name filter (optional)

**Valid Values:**
- `binance`
- `bybit`
- `hyperliquid`
- `kucoin`
- `paradex`
- `extended`
- `lighter`
- `aster`
- `pacifica`
- `nado`

**Behavior:**
- If provided: Returns data for that specific exchange
- If not provided: Returns data from all exchanges for the specified symbol

---

### Symbol Parameter

**Name:** `symbol`
**Type:** string
**Required:** **Yes** (for most endpoints)
**Description:** Trading symbol

**Format:**
- Alphanumeric only (max 20 characters)
- Case-insensitive (btc, BTC, bTc all work)
- Examples: `BTC`, `ETH`, `SOL`, `HYPE`, `DOGE`

**Behavior:**
- If exchange also provided: Returns data for that specific exchange/symbol combination
- If exchange not provided: Returns data for this symbol across all exchanges

---

### Time Range Parameters

**Start Time:**
- **Name:** `start`
- **Type:** integer (Unix timestamp in seconds)
- **Required:** No
- **Default:** 24 hours ago
- **Example:** `1733788800`

**End Time:**
- **Name:** `end`
- **Type:** integer (Unix timestamp in seconds)
- **Required:** No
- **Default:** Current time
- **Example:** `1733875200`

---

### Pagination Parameters

**Limit:**
- **Name:** `limit`
- **Type:** integer
- **Required:** No
- **Range:** 1-500
- **Default:** 100
- **Description:** Maximum number of records to return

**Offset:**
- **Name:** `offset`
- **Type:** integer
- **Required:** No
- **Default:** 0
- **Description:** Number of records to skip (for pagination)

**Example Pagination:**

```bash
# Get first page (records 0-99)
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&limit=100&offset=0"

# Get second page (records 100-199)
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&limit=100&offset=100"

# Get third page (records 200-299)
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&limit=100&offset=200"
```

---

## Error Handling

### Error Response Format

All errors return a JSON object with the following structure:

```json
{
  "error": "BadRequest",
  "message": "symbol parameter is required",
  "details": null
}
```

**Error Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `error` | string | Error type |
| `message` | string | Human-readable error message |
| `details` | string | Additional error details (nullable, only in development) |

### Error Types

| Error Type | HTTP Status | Description |
|------------|-------------|-------------|
| `BadRequest` | 400 | Missing or invalid parameters |
| `NotFound` | 404 | Resource does not exist |
| `DatabaseError` | 500 | Database query error |
| `NotImplemented` | 501 | Endpoint not yet implemented |
| `InternalError` | 500 | Internal server error |

### Common Error Scenarios

**Missing Required Parameter:**

```json
{
  "error": "BadRequest",
  "message": "symbol parameter is required",
  "details": null
}
```

**Invalid Parameter Value:**

```json
{
  "error": "BadRequest",
  "message": "Invalid exchange name",
  "details": null
}
```

**Database Error:**

```json
{
  "error": "DatabaseError",
  "message": "An error occurred while querying the database",
  "details": null
}
```

---

## Data Types

### Response Wrappers

#### DataResponse

Used for single data object responses.

```json
{
  "data": { /* ... data object or array ... */ },
  "timestamp": "2025-12-10T13:30:05Z"
}
```

**Fields:**
- `data`: Response data (can be null if no data available)
- `timestamp`: Response timestamp (ISO 8601 format)

#### PaginatedResponse

Used for paginated list responses.

```json
{
  "data": [ /* ... array of data objects ... */ ],
  "pagination": {
    "total": null,
    "limit": 100,
    "offset": 0,
    "count": 50
  }
}
```

**Pagination Fields:**
- `total`: Total number of records (null if not computed)
- `limit`: Maximum records per page
- `offset`: Number of records skipped
- `count`: Number of records returned in this response

### Number Precision

**All numeric values are returned as strings to preserve precision.**

This is especially important for cryptocurrency prices and amounts where precision loss could occur with floating-point numbers.

**Examples:**
- ✅ `"98234.50"` (string - preserves precision)
- ❌ `98234.50` (number - may lose precision)

### Timestamps

**Two timestamp formats are used:**

1. **ISO 8601 (datetime strings):**
   - Format: `YYYY-MM-DDTHH:MM:SS.SSSSSSSZ`
   - Example: `2025-12-10T13:30:00Z`
   - Used for: Data timestamps in responses

2. **Unix Timestamps (integers):**
   - Format: Seconds since Unix epoch
   - Example: `1733875200`
   - Used for: Query parameters (`start`, `end`)

**Converting Unix to ISO 8601:**

```bash
# Using date command (macOS/Linux)
date -r 1733875200

# Using JavaScript
new Date(1733875200 * 1000).toISOString()

# Using Python
from datetime import datetime
datetime.fromtimestamp(1733875200).isoformat()
```

### Basis Points (bps)

Liquidity depth is measured at fixed basis point spreads from the mid-price.

**1 basis point = 0.01% = 0.0001**

**Spread Levels:**
- `1bps` = 0.01% from mid-price
- `2.5bps` = 0.025% from mid-price
- `5bps` = 0.05% from mid-price
- `10bps` = 0.10% from mid-price
- `20bps` = 0.20% from mid-price

**Example:**
If mid-price is $100,000:
- `bid_1bps` = liquidity at $99,990 (100,000 * 0.9999)
- `ask_1bps` = liquidity at $100,010 (100,000 * 1.0001)

---

## Rate Limiting

Currently, there are **no rate limits** enforced by the API itself.

For production deployments, it is recommended to implement rate limiting at the reverse proxy level (e.g., Nginx, Apache, or cloud load balancer).

**Recommended Limits:**
- 100 requests per second per IP
- 10,000 requests per hour per IP

---

## Best Practices

### 1. Use Specific Queries

Always specify the symbol parameter to get targeted results:

```bash
# ✅ Good - specific query
curl "http://localhost:8080/api/v1/tickers?symbol=BTC&exchange=binance"

# ⚠️ Less efficient - returns data from all exchanges
curl "http://localhost:8080/api/v1/tickers?symbol=BTC"
```

### 2. Implement Pagination

For large datasets, use pagination to avoid overwhelming the server:

```bash
# ✅ Good - use pagination for large datasets
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&limit=100&offset=0"

# ⚠️ Avoid - requesting too many records at once
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&limit=500"
```

### 3. Handle Timestamps Correctly

Always use Unix timestamps (seconds) for query parameters:

```bash
# ✅ Good - Unix timestamp in seconds
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&start=1733788800"

# ❌ Wrong - ISO 8601 format not supported in query params
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&start=2025-12-10T00:00:00Z"
```

### 4. Parse Numeric Values as Strings

In your client code, parse numeric strings carefully to avoid precision loss:

```javascript
// ✅ Good - use decimal libraries
const price = new Decimal(response.data[0].last_price);

// ❌ Avoid - floating point may lose precision
const price = parseFloat(response.data[0].last_price);
```

### 5. Check Response Status

Always check both HTTP status codes and error responses:

```javascript
const response = await fetch('http://localhost:8080/api/v1/tickers?symbol=BTC');

if (!response.ok) {
  const error = await response.json();
  console.error(`Error: ${error.message}`);
  return;
}

const data = await response.json();
// Process data...
```

---

## Example Use Cases

### 1. Monitor BTC Price Across Exchanges

```bash
# Get current BTC price from all exchanges
curl "http://localhost:8080/api/v1/tickers?symbol=BTC" | jq '.data[] | {exchange, last_price, timestamp}'
```

### 2. Compare Liquidity Depth

```bash
# Compare BTC liquidity at 10bps across exchanges
curl "http://localhost:8080/api/v1/liquidity?symbol=BTC" | jq '.data[] | {exchange, bid_10bps, ask_10bps}'
```

### 3. Analyze Historical Price Movement

```bash
# Get last 24 hours of BTC tickers from Binance
NOW=$(date +%s)
DAY_AGO=$((NOW - 86400))
curl "http://localhost:8080/api/v1/tickers/history?symbol=BTC&exchange=binance&start=$DAY_AGO&end=$NOW"
```

### 4. Calculate Average Slippage

```bash
# Get slippage for $10k BTC trade on all exchanges
curl "http://localhost:8080/api/v1/slippage?symbol=BTC&amount=10000" | jq '.data[] | {exchange, buy_slippage_bps, sell_slippage_bps}'
```

### 5. Monitor Exchange Fees

```bash
# List all exchanges with their fees
curl "http://localhost:8080/api/v1/exchanges" | jq '.[] | {name, maker_fee, taker_fee}'
```

---

## Support & Contributing

For issues, questions, or feature requests, please visit:

**GitHub Repository:** [https://github.com/LampardNguyen234/perps-stats](https://github.com/LampardNguyen234/perps-stats)

---

## Changelog

### v0.1.0 (2025-12-11)
- Initial API release
- Endpoints: Health, Stats, Exchanges, Symbols, Tickers, Liquidity, Orderbooks, Slippage
- Multi-exchange support for 10 exchanges
- Historical data queries with pagination
- Symbol parameter now required for data endpoints
- Interactive API documentation at `/api/`

---

**Last Updated:** 2025-12-11
**API Version:** 0.1.0
