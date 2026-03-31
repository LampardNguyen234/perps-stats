# Adding a New Perpetual DEX Exchange

This document describes all the requirements and checklist for integrating a new perpetual futures DEX into the perps-stats system.

## Overview

When adding a new exchange, you must ensure:
1. **Trait Implementation**: Full `IPerps` implementation (REST API is mandatory; WebSocket is optional — see § 1.2)
2. **CLI Commands**: The following commands must work: `start`, `liquidity`, `market`, `ticker`, `serve`
3. **API Integration**: Both REST API backend and frontend UI must be updated
4. **Database**: Exchange must be registered with fees in the database

---

## Phase 1: Exchange Implementation

### 1.1 Create Exchange Module

**Location:** `crates/perps-exchanges/src/<exchange_name>/`

**Required Files:**
- `mod.rs` - Module entry point and exports
- `client.rs` - Main `IPerps` implementation (REST API client)
- `types.rs` - Exchange-specific API response types
- `conversions.rs` - Converters from exchange types to `perps_core` domain types

**Directory Structure:**
```
crates/perps-exchanges/src/<exchange_name>/
├── mod.rs           # Exports and module setup
├── client.rs        # IPerps implementation (~300-500 lines)
├── types.rs         # Serde structs for API responses
└── conversions.rs   # Exchange → perps_core type converters
```

### 1.2 Implement IPerps Trait

**Location:** `crates/perps-exchanges/src/<exchange_name>/client.rs`

The `IPerps` trait requires implementing 12 methods. Implement **all 12 methods**; do not leave stubs.

**Required Methods (Phase 2 - Core):**
```rust
pub struct ExchangeClient { /* fields */ }

#[async_trait::async_trait]
impl IPerps for ExchangeClient {
    fn get_name(&self) -> &str { "exchange_name" }

    async fn get_markets(&self) -> anyhow::Result<Vec<Market>> { /* ... */ }
    async fn get_ticker(&self, symbol: &str) -> anyhow::Result<Ticker> { /* ... */ }
    async fn get_all_tickers(&self) -> anyhow::Result<Vec<Ticker>> { /* ... */ }
    async fn get_orderbook(&self, symbol: &str, depth: u32) -> anyhow::Result<MultiResolutionOrderbook> { /* ... */ }
}
```

**Extended Methods (Phase 3 - History & Analytics):**
```rust
    async fn get_funding_rate(&self, symbol: &str) -> anyhow::Result<FundingRate> { /* ... */ }
    async fn get_funding_rate_history(
        &self, symbol: &str, start_time: Option<DateTime<Utc>>, end_time: Option<DateTime<Utc>>, limit: Option<u32>
    ) -> anyhow::Result<Vec<FundingRate>> { /* ... */ }

    async fn get_open_interest(&self, symbol: &str) -> anyhow::Result<OpenInterest> { /* ... */ }

    async fn get_klines(
        &self, symbol: &str, interval: &str, start_time: Option<DateTime<Utc>>, end_time: Option<DateTime<Utc>>, limit: Option<u32>
    ) -> anyhow::Result<Vec<Kline>> { /* ... */ }

    async fn get_recent_trades(&self, symbol: &str, limit: u32) -> anyhow::Result<Vec<Trade>> { /* ... */ }

    async fn get_market_stats(&self, symbol: &str) -> anyhow::Result<MarketStats> { /* ... */ }
    async fn get_all_market_stats(&self) -> anyhow::Result<Vec<MarketStats>> { /* ... */ }
```

**WebSocket Trait (Optional — requires user confirmation):**

> ⚠️ **Before implementing WebSocket:** Confirm with the user whether streaming support is needed.
> WebSocket adds significant complexity (reconnect logic, message parsing, heartbeats).
> Currently only Binance has WebSocket support (`BinanceWsClient`). Do NOT implement it
> speculatively — the REST path is fully sufficient for batch collection.

If confirmed, implement `IPerpsStream`:
```rust
#[async_trait::async_trait]
impl IPerpsStream for ExchangeClient {
    async fn stream_tickers(&self, symbols: Vec<&str>, on_data: Box<dyn Fn(Ticker) + Send>) -> anyhow::Result<()> { /* ... */ }
    async fn stream_orderbook(&self, symbols: Vec<&str>, on_data: Box<dyn Fn(OrderBook) + Send>) -> anyhow::Result<()> { /* ... */ }
    async fn stream_trades(&self, symbols: Vec<&str>, on_data: Box<dyn Fn(Trade) + Send>) -> anyhow::Result<()> { /* ... */ }
    async fn stream_funding_rate(&self, symbols: Vec<&str>, on_data: Box<dyn Fn(FundingRate) + Send>) -> anyhow::Result<()> { /* ... */ }
}
```

### 1.3 Symbol Mapping

**Requirement:** Implement `parse_symbol()` and `normalize_symbol()` — the two symbol conversion
methods declared in the `IPerps` trait. These must be inverses of each other.

**Location:** `crates/perps-exchanges/src/<exchange_name>/client.rs`

#### `parse_symbol` — global → exchange format

```rust
fn parse_symbol(&self, symbol: &str) -> String {
    // 1. Apply alias (e.g. "XAU" → "GOLD" on qfex); no-op if no alias defined.
    let resolved = perps_exchanges::resolve_alias("exchange_name", symbol);
    // 2. Strip the suffix if already present (idempotency).
    let upper = resolved.to_uppercase();
    let base = upper.trim_end_matches("_USDT_PERP");
    // 3. Append exchange-specific suffix.
    format!("{}_USDT_PERP", base)
}
```

> ⚠️ **Idempotency requirement:** `parse_symbol` MUST be idempotent. Passing an already-valid
> exchange symbol must return it unchanged. Always strip known suffixes before appending:
> ```rust
> fn parse_symbol(&self, symbol: &str) -> String {
>     let upper = symbol.to_uppercase();
>     let base = upper
>         .trim_end_matches("/USDT-P")   // strip if already in exchange format
>         .trim_end_matches("USDT")
>         .trim_end_matches("-USDT");
>     format!("{}/USDT-P", base)
> }
> ```

#### `normalize_symbol` — exchange format → global

`normalize_symbol` is the **inverse** of `parse_symbol`. It is the real trait method name
(not `reverse_parse_symbol`).

```rust
fn normalize_symbol(&self, exchange_symbol: &str) -> String {
    // 1. Strip exchange-specific suffix to get the base name.
    let base = exchange_symbol
        .trim_end_matches("_USDT_PERP")
        .to_uppercase();
    // 2. Reverse-apply alias (e.g. "GOLD" → "XAU" on qfex); no-op if no alias defined.
    perps_exchanges::unresolve_alias("exchange_name", &base).to_string()
}
```

**Round-trip guarantee:** `normalize_symbol(parse_symbol(global)) == global` for all valid
global symbols. Add a unit test for this (see §8.1).

#### Symbol Aliases (`symbol_aliases.toml`)

Some exchanges use base names that differ from the project's global symbol names
(e.g., QFEX calls gold `"GOLD"` while the project uses `"XAU"`). Handle these via the
central alias file rather than hardcoding logic in the client.

**File:** `symbol_aliases.toml` (project root)

```toml
[exchange_name]
# GLOBAL_KEY = "exchange_base_name"
XAU = "GOLD"    # global "XAU" ↔ exchange "GOLD"
XAG = "SILVER"
```

- `resolve_alias(exchange, global_sym)` — used in `parse_symbol` to map global → exchange base
- `unresolve_alias(exchange, exchange_base)` — used in `normalize_symbol` to map exchange → global
- Aliases are loaded at startup via `init_aliases(path)` in `src/main.rs` (already called)
- Add entries here, not in client code, whenever a global name differs from the exchange base

#### `is_supported`

Delegate to `get_markets()`. This is the standard pattern across all clients. Do **not** make
a separate raw HTTP call — it duplicates endpoint logic, misses the `ACTIVE` status filter
already applied in `get_markets()`, and bypasses any caching.

```rust
async fn is_supported(&self, symbol: &str) -> anyhow::Result<bool> {
    let markets = self.get_markets().await?;
    let exchange_sym = self.parse_symbol(symbol);
    Ok(markets.iter().any(|m| m.symbol == exchange_sym))
}
```

### 1.4 Rate Limiting

**Requirement:** Implement client-side rate limiting. This is critical — failing to rate-limit
will DDoS the exchange API, which will result in IP bans and data gaps.

**Location:** `crates/perps-exchanges/src/<exchange_name>/client.rs`

```rust
use perps_core::rate_limiter::RateLimiter;

pub struct ExchangeClient {
    base_url: String,
    rate_limiter: Arc<RateLimiter>,
    // ... other fields
}

impl ExchangeClient {
    pub fn new() -> Self {
        // Research the exchange's actual rate limit before setting this number.
        // Example: Hibachi allows 300 requests per 10-second sliding window → use RateLimiter::hibachi()
        // Example: Gravity allows 50 requests per second → RateLimiter::new(50, Duration::from_secs(1))
        let rate_limiter = Arc::new(RateLimiter::new(50, std::time::Duration::from_secs(1)));
        Self {
            base_url: "https://api.example.com".to_string(),
            rate_limiter,
            // ..
        }
    }
}
```

**API call budgeting per method:**

Before implementing `get_all_tickers()`, calculate the total API calls required:
- If each ticker requires N separate endpoints (e.g., `/prices` + `/stats` + `/open-interest` = 3 calls),
  and there are M active markets, then `get_all_tickers()` costs **N × M** requests per invocation.
- For 30 markets at 3 calls each = 90 requests. At 50 req/s this takes ~2 seconds. At 5 req/s (conservative
  limit) this takes 18 seconds — likely too slow for batch collection.
- **Solution:** Use `tokio::try_join!` for per-symbol parallel calls and `join_all` across symbols.
  The rate limiter will throttle automatically.

```rust
// Per-symbol: fetch all needed endpoints in parallel
async fn fetch_ticker_data(&self, symbol: &str) -> Result<(PricesResponse, StatsResponse, OIResponse)> {
    let (prices, stats, oi) = tokio::try_join!(
        self.fetch_prices(symbol),
        self.fetch_stats(symbol),
        self.fetch_oi(symbol),
    )?;
    Ok((prices, stats, oi))
}

// Across symbols: fan out with join_all (rate limiter throttles)
async fn get_all_tickers(&self) -> Result<Vec<Ticker>> {
    let symbols = self.get_live_symbols().await?;
    let futures: Vec<_> = symbols.iter().map(|sym| self.get_ticker(sym)).collect();
    let results = join_all(futures).await;
    Ok(results.into_iter().flatten().collect()) // skip failures, log them
}
```

### 1.5 Response Caching

**Requirement:** Add a short-lived in-memory response cache when an exchange has per-symbol
endpoints. Without caching, `get_ticker` + `get_all_tickers` called close together will
double the API cost.

**When to use:** If you call multiple endpoints per symbol (e.g., `/prices`, `/stats`, `/open-interest`
separately), a 3–10 second TTL cache dramatically reduces API cost during batch cycles.

```rust
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const RESPONSE_CACHE_TTL: Duration = Duration::from_secs(5);

pub struct ExchangeClient {
    // ...
    response_cache: Arc<RwLock<HashMap<String, (Instant, String)>>>,
}

async fn get<R: serde::de::DeserializeOwned + Send + 'static>(
    &self, path: &str, query: &[(&str, &str)]
) -> Result<R> {
    let key = Self::cache_key(path, query);

    // Fast path
    {
        let cache = self.response_cache.read().await;
        if let Some((inserted_at, body)) = cache.get(&key) {
            if inserted_at.elapsed() < RESPONSE_CACHE_TTL {
                return serde_json::from_str(body).with_context(|| format!("deserialize cached {}", key));
            }
        }
    }

    // Cache miss: fetch and store
    let body = self.fetch_raw(path, query).await?;
    self.response_cache.write().await.insert(key, (Instant::now(), body.clone()));
    serde_json::from_str(&body).with_context(|| format!("deserialize {}{}", self.base_url, path))
}
```

### 1.6 Retry Policy

**Requirement:** Use `execute_with_retry` for all HTTP calls to handle transient network errors.

```rust
use perps_core::{execute_with_retry, RetryConfig};

async fn fetch_raw(&self, path: &str, query: &[(&str, &str)]) -> Result<String> {
    let config = RetryConfig::default();
    execute_with_retry(&config, || async {
        self.rate_limiter.execute(|| async {
            let resp = self.http.get(&url).query(query).send().await?;
            if !resp.status().is_success() {
                return Err(anyhow!("HTTP {}: {}", resp.status(), resp.text().await?));
            }
            Ok(resp.text().await?)
        }).await
    }).await
}
```

### 1.7 Error Handling & Logging

**Requirement:** Use `anyhow::Result` with `.context()` for meaningful error messages; use `tracing` for logging.

```rust
use anyhow::Context;
use tracing::{debug, warn, error};

async fn fetch_ticker(&self, symbol: &str) -> anyhow::Result<Ticker> {
    debug!("Fetching ticker for symbol: {}", symbol);

    let exchange_symbol = self.parse_symbol(symbol);
    let response = self.make_request(&format!("/ticker/{}", exchange_symbol))
        .await
        .context("failed to fetch ticker from API")?;

    let ticker: TickerResponse = serde_json::from_str(&response)
        .context("failed to parse ticker response")?;

    Ok(ticker.into())
}
```

---

## Phase 2: Field Semantics — Critical

This is the most common source of bugs. Every exchange uses different terminology for
the same concepts. Always verify the field semantics against the API docs before mapping.

### 2.1 Volume vs Turnover

The `Ticker` and `MarketStats` types have two distinct volume fields:

| Field | Type | Meaning |
|---|---|---|
| `volume_24h` | `Decimal` | Base asset quantity traded in 24h (e.g., BTC amount) |
| `turnover_24h` | `Decimal` | Quote/USD notional traded in 24h (e.g., USD value) |

These are **not interchangeable**. The relationship is: `turnover_24h = volume_24h × price`

**How to identify which the API is returning:**

Ask: *Does the number scale with the number of contracts (small), or the USD value (large)?*
- For BTC at \$95,000: `volume_24h` ~ 1,000–100,000 BTC; `turnover_24h` ~ \$100M–\$10B
- If the exchange field is named `volume` but the value is in the billions → it's turnover.
- If the exchange field is named `quoteVolume`, `volumeUSD`, `notional`, `turnover` → it's turnover.

**Conversion patterns:**

```rust
// Case 1: API provides turnover (USD) only — derive base volume
// e.g., Hibachi's stats.volume_24h is USD turnover
let turnover_24h = parse_decimal_opt(&stats.volume_24h, "volume_24h");
let volume_24h = if last_price > Decimal::ZERO {
    turnover_24h / last_price
} else {
    Decimal::ZERO
};

// Case 2: API provides base volume only — derive turnover
// e.g., some exchanges only expose base volume
let volume_24h = parse_decimal_opt(&stats.volume, "volume");
let turnover_24h = volume_24h * last_price;

// Case 3: API provides both explicitly
// e.g., Gravity: buy_volume_24h_b (base) + sell_volume_24h_b (base), buy_volume_24h_q (quote) + sell...
let volume_24h = buy_base + sell_base;
let turnover_24h = buy_quote + sell_quote;
```

**Same rule applies to `Kline`:**
- `Kline.volume` = base asset quantity
- `Kline.turnover` = USD notional
- If the API provides `volume_notional` → that maps to `turnover`; derive base as `notional / close`

### 2.2 Open Interest

The `OpenInterest` and `Ticker` types have two OI fields:

| Field | Type | Meaning |
|---|---|---|
| `open_interest` | `Decimal` | Number of open contracts (base asset quantity) |
| `open_interest_notional` | `Decimal` | USD value of open contracts (OI × mark_price) |

If the API only provides quantity:
```rust
let open_interest = parse_decimal_opt(&oi.total_quantity, "total_quantity");
let open_interest_notional = if open_interest > Decimal::ZERO && mark_price > Decimal::ZERO {
    open_interest * mark_price
} else {
    Decimal::ZERO  // Acceptable fallback when mark_price unavailable
};
```

If the API only provides notional:
```rust
let open_interest_notional = parse_decimal_opt(&oi.notional, "notional");
let open_interest = if mark_price > Decimal::ZERO {
    open_interest_notional / mark_price
} else {
    Decimal::ZERO
};
```

### 2.3 Price Change Percentage

`price_change_pct` must be stored as a **decimal ratio**, not a percentage:
- ✅ Correct: `-0.0119` (means -1.19%)
- ❌ Wrong: `-1.19` (percentage form — will display as -119% in the UI)

```rust
let price_change_pct = if open_price > Decimal::ZERO {
    (last_price - open_price) / open_price   // e.g., -0.0119
} else {
    Decimal::ZERO
};
```

### 2.4 Timestamp Formats

Exchanges use wildly different timestamp formats. Always convert to `DateTime<Utc>`:

| API Format | Example Value | Conversion |
|---|---|---|
| Unix seconds | `1700000000` | `Utc.timestamp_opt(secs, 0)` |
| Unix milliseconds | `1700000000000` | `Utc.timestamp_millis_opt(ms)` |
| Unix nanoseconds | `1700000000000000000` | `nanos / 1_000_000_000` then seconds |
| ISO 8601 string | `"2024-11-14T22:13:20Z"` | `DateTime::parse_from_rfc3339` |

```rust
// Always use .single().unwrap_or_else(Utc::now) to avoid panics on invalid timestamps
fn unix_secs_to_datetime(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

fn unix_millis_to_datetime(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_else(Utc::now)
}
```

### 2.5 Funding Rate Format

`funding_rate` must be a **decimal ratio**, not a percentage or basis points:
- ✅ Correct: `0.0001` (means 0.01% per 8h)
- ❌ Wrong: `0.01` (that's 1% per 8h — 100× too large)
- ❌ Wrong: `1` (basis points form)

Always check a live BTC funding rate against a known source (typically ±0.0001 for calm markets).

---

## Phase 3: Orderbook — MultiResolutionOrderbook

### 3.1 When to Use MultiResolutionOrderbook

Some exchanges (e.g., Hibachi) provide orderbooks at multiple price granularities (tick sizes).
`MultiResolutionOrderbook` wraps multiple `Orderbook` instances at different granularities
so the aggregator can pick the best resolution for liquidity depth calculations.

**Decision guide:**
- Exchange has a single standard orderbook → use `MultiResolutionOrderbook::from_single(orderbook)`
- Exchange supports multiple granularities/tick sizes → fetch 2 (finest + one coarser) and use `MultiResolutionOrderbook::from_multiple(...)`

**Do not** fetch more than 2–3 granularities; the marginal benefit is low and the API cost is high.

```rust
// Single-granularity exchange (most common)
async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
    let ob = self.fetch_orderbook(symbol, depth).await?;
    let orderbook = conversions::to_orderbook(ob, symbol)?;
    Ok(MultiResolutionOrderbook::from_single(orderbook))
}

// Multi-granularity exchange (e.g., Hibachi)
async fn get_orderbook(&self, symbol: &str, depth: u32) -> Result<MultiResolutionOrderbook> {
    let granularities = self.get_granularities_for(symbol).await?;
    let (fine, coarse) = tokio::try_join!(
        self.fetch_orderbook_at(symbol, depth, &granularities[0]),
        self.fetch_orderbook_at(symbol, depth, &granularities[1]),
    )?;
    Ok(MultiResolutionOrderbook::from_multiple(
        symbol.to_string(),
        Utc::now(),
        vec![fine, coarse],
    ))
}
```

### 3.2 Orderbook Validation

Always ensure:
- Bids are sorted **descending** by price (highest first)
- Asks are sorted **ascending** by price (lowest first)
- No crossed book: `best_bid < best_ask`

```rust
bids.sort_by(|a, b| b.price.cmp(&a.price));  // descending
asks.sort_by(|a, b| a.price.cmp(&b.price));  // ascending

// Optional sanity check (log warning, don't fail)
if let (Some(bid), Some(ask)) = (bids.first(), asks.first()) {
    if bid.price >= ask.price {
        tracing::warn!("Crossed orderbook for {}: bid {} >= ask {}", symbol, bid.price, ask.price);
    }
}
```

### 3.3 Orderbook Depth Clamping

Always clamp the requested depth to the exchange's supported range:
```rust
let depth_clamped = depth.clamp(1, 100) as usize;  // Hibachi: [1, 100]
```

---

## Phase 4: Factory Registration

### 4.1 Register in Factory

**Location:** `crates/perps-exchanges/src/factory.rs`

**Step 1:** Add import
```rust
use crate::<exchange_name>::ExchangeClient;
```

**Step 2:** Add to `all_exchanges()` function
```rust
pub async fn all_exchanges() -> Vec<(String, Box<dyn IPerps + Send + Sync>)> {
    let mut exchanges = vec![
        // ... existing exchanges
        (
            "<exchange_name>".to_string(),
            Box::new(ExchangeClient::new()) as Box<dyn IPerps + Send + Sync>,
        ),
    ];
    exchanges
}
```

**Step 3:** Add to `get_exchange()` function
```rust
pub async fn get_exchange(name: &str) -> anyhow::Result<Box<dyn IPerps + Send + Sync>> {
    match name.to_lowercase().as_str() {
        // ... existing matches
        "<exchange_name>" => Ok(Box::new(ExchangeClient::new())),
        _ => anyhow::bail!("Unsupported exchange: {}. Currently supported: ...", name),
    }
}
```

### 4.2 Add Factory Tests

**Location:** `crates/perps-exchanges/src/factory.rs` (tests module)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_exchange_<exchange_name>() {
        let result = get_exchange("<exchange_name>").await;
        assert!(result.is_ok(), "get_exchange should succeed for <exchange_name>");
        let client = result.unwrap();
        assert_eq!(client.get_name(), "<exchange_name>");
    }

    #[tokio::test]
    async fn test_all_exchanges_includes_<exchange_name>() {
        let exchanges = all_exchanges().await;
        let found = exchanges.iter().any(|(name, _)| name == "<exchange_name>");
        assert!(found, "<exchange_name> should be in all_exchanges()");
    }
}
```

---

## Phase 5: Database Integration

### 5.1 Create Migration

**Location:** `migrations/NN_add_<exchange_name>_exchange.sql`

**Naming Convention:** Use timestamp format `YYYYMMDDHHMMSS_descriptive_name.sql`

**Pattern:**
```sql
-- Add <exchange_name> exchange
-- <exchange_name> is a perpetual futures DEX with API-based market data access

-- UP: Insert exchange if it doesn't already exist
INSERT INTO exchanges (name, maker_fee, taker_fee)
VALUES ('<exchange_name>', 0.0001, 0.0005)
ON CONFLICT (name) DO NOTHING;

-- DOWN: Delete exchange (if needed, uncomment for rollback)
-- DELETE FROM exchanges WHERE name = '<exchange_name>';
```

**Requirements:**
- Use `ON CONFLICT (name) DO NOTHING` for idempotency
- Include both maker and taker fees (research exchange fee schedule)
- Document the exchange briefly in comments
- Fees should be decimal values (e.g., 0.0001 = 0.01%)

### 5.2 Run Migration

```bash
# Verify migration is recognized
cargo sqlx prepare --database-url $DATABASE_URL

# Run migration
cargo run -- db migrate
```

### 5.3 Historical Data Fix Migrations

If a conversion bug is discovered after data has been stored, write a corrective migration:

```sql
-- Fix <exchange_name> tickers: <describe the bug>
-- <wrong field> was stored as <wrong value>; correct value is <correct formula>

UPDATE tickers
SET
    turnover_24h = volume_24h,              -- what was stored in volume_24h was actually turnover
    volume_24h   = CASE
                       WHEN last_price > 0 THEN volume_24h / last_price
                       ELSE 0
                   END
WHERE exchange = '<exchange_name>'
  AND volume_24h IS NOT NULL
  AND volume_24h > 0;
```

Always use a `WHERE exchange = '<exchange_name>'` clause — never update other exchanges' data.

---

## Phase 6: CLI Command Integration

All major CLI commands must work with the new exchange. Verify these commands:

### 6.1 Ticker Command

```bash
cargo run -- ticker --exchange <exchange_name> --symbols BTC,ETH
```

**Expected:** Display current ticker prices

**Code Location:** `src/commands/ticker.rs`
- Exchange is already dynamically loaded via `factory.rs`
- No changes needed if `IPerps` is properly implemented

### 6.2 Market Command

```bash
cargo run -- market --exchange <exchange_name> --symbols BTC
```

**Expected:** Display comprehensive market data (price, volume, funding rate, etc.)

**Code Location:** `src/commands/market.rs`
- Calls `get_market_stats()` method

### 6.3 Liquidity Command

```bash
cargo run -- liquidity --exchange <exchange_name> --symbols BTC,ETH
```

**Expected:** Display liquidity depth at various spreads (1, 2.5, 5, 10, 20 bps)

**Code Location:** `src/commands/liquidity.rs`
- Calls `get_orderbook()` method
- Aggregator calculates depth from orderbook
- If depth values are all zero, the orderbook is likely crossed or price levels are wrong

### 6.4 Start Command

```bash
cargo run -- start --exchanges <exchange_name> --symbols BTC,ETH --report-interval 60
```

**Expected:** Periodically fetch and store tickers and liquidity data

**Code Location:** `src/commands/start.rs`
- Uses `repository.store_tickers_with_exchange()` (never `store_tickers()`)
- Monitor logs for repeated rate-limit warnings — if present, lower parallelism or increase interval

### 6.5 Serve Command (API Server)

```bash
cargo run -- serve --port 8080
curl http://127.0.0.1:8080/api/v1/exchanges
```

**Expected:** API returns exchange list including new exchange from database

**Code Location:** `src/commands/serve/handlers/exchanges.rs`
- Queries database directly: `SELECT * FROM exchanges`
- No code changes needed (database-driven)

---

## Phase 7: API Frontend Update

### 7.1 Frontend Dynamic Loading

**Location:** `src/api/index.html`

The frontend automatically fetches exchanges from the API backend:

**Mechanism:**
1. Page loads → calls `loadExchanges()` function
2. `loadExchanges()` fetches from `/api/v1/exchanges` endpoint
3. All dropdown menus populated from API response
4. No hardcoded lists in HTML

**Verification:**
```bash
# 1. Start the API server
cargo run -- serve --port 8080 &

# 2. Open browser
open http://127.0.0.1:8080/api/v1

# 3. Verify exchange appears in "Select Exchange name" dropdown
# 4. Verify exchange appears in all endpoint parameter dropdowns
```

**No Manual Changes Needed:**
- Once migration runs, new exchange appears in database
- API endpoint returns new exchange
- Frontend automatically displays it (dynamic loading)

---

## Phase 8: Testing

### 8.1 Unit Tests for Conversions

**Minimum required tests in `conversions.rs`:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // 1. Test symbol parsing (idempotency is critical)
    #[test]
    fn test_parse_symbol_btc() {
        assert_eq!(ExchangeClient::to_exchange_symbol("BTC"), "BTC-PERP");
        assert_eq!(ExchangeClient::to_exchange_symbol("BTC-PERP"), "BTC-PERP"); // idempotent
        assert_eq!(ExchangeClient::to_exchange_symbol("btc"), "BTC-PERP");      // case insensitive
    }

    // 2. Test volume/turnover semantics explicitly
    #[test]
    fn test_ticker_volume_turnover_semantics() {
        // Given: API returns volume_24h = 1_000_000 (this is USD turnover, not base qty)
        let ticker = convert_ticker(&mock_api_response());
        let btc_price = dec!(95_000);

        // turnover_24h should equal the raw API value (USD)
        assert_eq!(ticker.turnover_24h, dec!(1_000_000));
        // volume_24h should be turnover / price (base quantity)
        assert!((ticker.volume_24h - dec!(1_000_000) / btc_price).abs() < dec!(0.001));
        // Sanity: volume_24h should be much smaller than turnover_24h for BTC
        assert!(ticker.volume_24h < ticker.turnover_24h);
    }

    // 3. Test open interest notional derivation
    #[test]
    fn test_open_interest_notional() {
        let ticker = convert_ticker(&mock_api_response()); // OI qty = 50.5 BTC, mark_price = 95010
        assert_eq!(ticker.open_interest, dec!(50.5));
        assert!(ticker.open_interest_notional > dec!(4_000_000)); // ~50.5 × 95010
    }

    // 4. Test price_change_pct is decimal ratio not percentage
    #[test]
    fn test_price_change_pct_is_ratio() {
        let ticker = convert_ticker_with_open(&mock_api_response(), open_price = dec!(95000), last_price = dec!(94050));
        // Should be ~-0.01 (ratio), not ~-1.0 (percentage)
        assert!(ticker.price_change_pct > dec!(-0.1) && ticker.price_change_pct < dec!(0.0));
    }

    // 5. Test kline volume/turnover
    #[test]
    fn test_kline_volume_from_notional() {
        // If API provides volume_notional = 9_500_000 and close = 95_000
        // then volume (base) = 9_500_000 / 95_000 = 100
        let kline = convert_kline(&mock_kline(volume_notional: "9500000", close: "95000"));
        assert_eq!(kline.volume, dec!(100));
        assert_eq!(kline.turnover, dec!(9_500_000));
    }

    // 6. Test normalize_symbol round-trip
    #[test]
    fn test_normalize_symbol_roundtrip() {
        let c = ExchangeClient::new();
        assert_eq!(c.normalize_symbol(&c.parse_symbol("BTC")), "BTC");
        assert_eq!(c.normalize_symbol(&c.parse_symbol("ETH")), "ETH");
    }

    // 7. Test alias round-trips (only if exchange has entries in symbol_aliases.toml)
    #[test]
    fn test_alias_roundtrip() {
        let aliases_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../symbol_aliases.toml");
        perps_exchanges::init_aliases(&aliases_path);
        let c = ExchangeClient::new();
        // e.g. on qfex: XAU → GOLD-USD → XAU
        assert_eq!(c.parse_symbol("XAU"), "GOLD-USD");
        assert_eq!(c.normalize_symbol("GOLD-USD"), "XAU");
    }

    // 8. Test orderbook ordering
    #[test]
    fn test_orderbook_sorted_correctly() {
        let ob = convert_orderbook(&mock_ob_response());
        // Bids descending
        for i in 0..ob.bids.len().saturating_sub(1) {
            assert!(ob.bids[i].price >= ob.bids[i+1].price);
        }
        // Asks ascending
        for i in 0..ob.asks.len().saturating_sub(1) {
            assert!(ob.asks[i].price <= ob.asks[i+1].price);
        }
        // No crossed book
        if let (Some(bid), Some(ask)) = (ob.bids.first(), ob.asks.first()) {
            assert!(bid.price < ask.price);
        }
    }
}
```

### 8.2 Integration Tests

**Location:** `crates/perps-exchanges/tests/<exchange_name>_integration_tests.rs`

**Pattern:**
```rust
#[tokio::test]
async fn test_get_ticker_integration() {
    let client = ExchangeClient::new();
    let ticker = client.get_ticker("BTC").await;

    // Gracefully handle API unavailability
    match ticker {
        Ok(t) => {
            assert_eq!(t.symbol, "BTC");
            assert!(t.last_price > Decimal::ZERO);
            // Verify volume/turnover relationship
            if t.last_price > Decimal::ZERO && t.volume_24h > Decimal::ZERO {
                let implied_turnover = t.volume_24h * t.last_price;
                let ratio = (implied_turnover - t.turnover_24h).abs() / t.turnover_24h;
                assert!(ratio < dec!(0.05), "volume/turnover mismatch: implied {}, actual {}", implied_turnover, t.turnover_24h);
            }
        }
        Err(e) => {
            eprintln!("API unavailable or symbol not found: {}", e);
            // Test passes if API is unavailable
        }
    }
}
```

### 8.3 Full Integration Test Checklist

```bash
# 1. Build and check
cargo build --release
cargo check
cargo clippy

# 2. Run all tests
cargo test
cargo test <exchange_name>              # Exchange-specific tests
cargo test factory::tests               # Factory tests

# 3. Run CLI commands
cargo run -- ticker --exchange <exchange_name> --symbols BTC,ETH
cargo run -- market --exchange <exchange_name> --symbols BTC
cargo run -- liquidity --exchange <exchange_name> --symbols BTC

# 4. Test batch collection (5 min)
DATABASE_URL=postgres://... cargo run -- start --exchanges <exchange_name> --symbols BTC --report-interval 10

# 5. Test API server
cargo run -- serve --port 8080 &
curl http://127.0.0.1:8080/api/v1/exchanges | jq '.[] | select(.name == "<exchange_name>")'
```

---

## Checklist for New Exchange Integration

### Exchange Implementation
- [ ] Module created: `crates/perps-exchanges/src/<exchange_name>/`
- [ ] `client.rs`: Full `IPerps` implementation (all 12 methods)
- [ ] `types.rs`: Serde structs for API responses
- [ ] `conversions.rs`: Type converters to `perps_core` types
- [ ] `mod.rs`: Proper exports
- [ ] `parse_symbol()` implemented — idempotent (calling twice gives same result)
- [ ] `parse_symbol()` calls `resolve_alias(exchange_name, symbol)` for alias support
- [ ] `normalize_symbol()` inverts `parse_symbol()` — round-trip test passes
- [ ] `normalize_symbol()` calls `unresolve_alias(exchange_name, base)` for alias support
- [ ] `is_supported()` delegates to `get_markets()` — does NOT make a raw HTTP call
- [ ] Symbol aliases added to `symbol_aliases.toml` if exchange uses non-standard base names
- [ ] Rate limiting implemented and rate limit researched from API docs
- [ ] Response caching implemented (if exchange has per-symbol endpoints)
- [ ] Retry logic uses `execute_with_retry`
- [ ] Error handling uses `.context()` and `anyhow::Result`
- [ ] No `unwrap()` in production code
- [ ] Logging uses `tracing` macros (`debug!`, `info!`, `warn!`, `error!`)
- [ ] Public functions have doc comments
- [ ] WebSocket support **only if explicitly confirmed with user**

### API Response Types (`types.rs`) Verification
- [ ] `curl`-ed every endpoint used before writing any serde struct (do not infer naming convention from docs)
- [ ] Each endpoint's naming convention noted individually (snake_case vs camelCase may differ per endpoint on the same exchange)
- [ ] `rename_all` applied only when ALL fields of that struct follow the same convention; otherwise use per-field `#[serde(rename = "...")]`
- [ ] Numeric fields that can appear as `""` (empty string) in API responses declared as `Option<String>`, not `Option<f64>`
- [ ] After writing types, did a sanity `serde_json::from_str` test against a real API response body (not just a compile check)

### Field Semantics Verification
- [ ] `volume_24h` = base asset quantity (NOT USD notional)
- [ ] `turnover_24h` = USD notional (NOT base quantity)
- [ ] `open_interest` = contract/base quantity
- [ ] `open_interest_notional` = USD value (derived from qty × mark_price if not in API)
- [ ] `price_change_pct` is decimal ratio (e.g., -0.01 for -1%), not percentage (-1.0)
- [ ] `funding_rate` is decimal ratio (e.g., 0.0001 for 0.01%), not basis points or percentage
- [ ] Timestamps converted correctly (check: seconds vs milliseconds vs nanoseconds)
- [ ] Kline `volume` = base quantity; kline `turnover` = USD notional
- [ ] Sanity-checked live values against known sources (e.g., CoinGlass, exchange UI)
- [ ] All derived Decimal divisions use `.round_dp(6)` for volumes and `.round_dp(2)` for USD notional

### Orderbook Verification
- [ ] Bids sorted descending (highest bid first)
- [ ] Asks sorted ascending (lowest ask first)
- [ ] No crossed book in normal market conditions
- [ ] Depth clamped to exchange's supported range
- [ ] MultiResolutionOrderbook used correctly: `from_single` or `from_multiple`
- [ ] If multi-granularity: fetched at most 2 granularities (finest + one coarser)

### API Optimization
- [ ] `get_all_tickers()` uses `join_all` for parallel symbol fetching
- [ ] Per-symbol multi-endpoint fetches use `tokio::try_join!`
- [ ] Total API calls per batch cycle estimated and within rate limit budget
- [ ] Response cache TTL set to 3–10 seconds for frequently-polled endpoints
- [ ] Partial failures in `get_all_*` methods are logged and skipped (not fatal)

### Factory Registration
- [ ] Import added to `crates/perps-exchanges/src/factory.rs`
- [ ] Added to `all_exchanges()` function
- [ ] Added to `get_exchange()` function
- [ ] Factory tests added and passing

### Database Integration
- [ ] Migration file created: `migrations/NN_add_<exchange_name>_exchange.sql`
- [ ] Migration includes maker and taker fees
- [ ] Migration uses `ON CONFLICT (name) DO NOTHING`
- [ ] Migration run successfully: `cargo run -- db migrate`

### CLI Commands
- [ ] `cargo run -- ticker --exchange <exchange_name> --symbols BTC` works
- [ ] `cargo run -- market --exchange <exchange_name> --symbols BTC` works
- [ ] `cargo run -- liquidity --exchange <exchange_name> --symbols BTC` works — depth values non-zero
- [ ] `cargo run -- start --exchanges <exchange_name>` works and stores data
- [ ] API server starts and returns exchange in `/api/v1/exchanges`

### API Frontend
- [ ] Frontend loads exchanges dynamically from API
- [ ] Exchange appears in all dropdown menus
- [ ] No hardcoded exchange lists in HTML

### Docker Deployment
- [ ] Docker image builds successfully: `docker build -t perps-stats:latest .`
- [ ] All tests pass in Docker container: `docker run --rm perps-stats cargo test --all`
- [ ] Clippy passes in Docker: `docker run --rm perps-stats cargo clippy --all`
- [ ] Code format is correct in Docker: `docker run --rm perps-stats cargo fmt --check`
- [ ] Docker Compose file works: `docker-compose up -d && docker-compose down`
- [ ] New exchange available in Docker container ticker: `docker run perps-stats cargo run -- ticker --exchange <exchange_name> --symbols BTC`
- [ ] Migration runs successfully in Docker container
- [ ] API server accessible from Docker container on port 8080

### Testing
- [ ] Unit tests for symbol parsing (including idempotency)
- [ ] Unit test for `normalize_symbol` round-trip: `normalize_symbol(parse_symbol("BTC")) == "BTC"`
- [ ] Unit tests for alias round-trips (if exchange has entries in `symbol_aliases.toml`)
- [ ] Unit tests explicitly asserting volume vs turnover semantics
- [ ] Unit tests for open interest notional derivation
- [ ] Unit tests for orderbook sort order
- [ ] Integration tests for all `IPerps` methods
- [ ] Integration tests assert **non-zero values** for `mark_price`, `turnover_24h` (not just symbol name / list not empty)
- [ ] Factory tests added
- [ ] All tests pass: `cargo test`
- [ ] Clippy passes: `cargo clippy`
- [ ] Format passes: `cargo fmt --check`

### Documentation
- [ ] README updated with new exchange in supported exchanges table
- [ ] Known limitations documented (e.g., no funding rate history, OI notional not available)

---

## Common Pitfalls

### 1. **Using Stub Methods**
❌ **Wrong:** Call `store_tickers()` (panics)
```rust
repo.store_tickers(&tickers).await?;  // PANIC!
```

✅ **Right:** Call `store_tickers_with_exchange()`
```rust
repo.store_tickers_with_exchange(&exchange_name, &tickers).await?;
```

### 2. **Confusing Volume and Turnover**
❌ **Wrong:** Treat API's `volume_24h` field as base quantity when it's actually USD notional
```rust
// Bug: stats.volume_24h is USD turnover but treated as base volume
let volume_24h = parse_decimal(&stats.volume_24h);
let turnover_24h = volume_24h * last_price;  // Now turnover is ~100× too large
```

✅ **Right:** Read the API docs; verify with a sanity check (BTC volume should be thousands, not billions)
```rust
// When API provides USD turnover:
let turnover_24h = parse_decimal(&stats.volume_24h);    // USD notional (the large number)
let volume_24h = turnover_24h / last_price;              // Base qty (the small number)
```

### 3. **Not Implementing All 12 Methods**
❌ **Wrong:** Leave methods returning `Err(anyhow::anyhow!("not implemented"))`

✅ **Right:** Implement all 12 methods or gracefully handle unsupported features
```rust
async fn get_funding_rate_history(...) -> anyhow::Result<Vec<FundingRate>> {
    // If API doesn't support history, return current rate as single element
    let current = self.get_funding_rate(symbol).await?;
    Ok(vec![FundingRate { symbol: symbol.to_string(), funding_rate: current.funding_rate, .. }])
}
```

### 4. **Non-Idempotent Symbol Parsing**
❌ **Wrong:** Blindly append suffix without stripping it first
```rust
fn parse_symbol(symbol: &str) -> String {
    format!("{}/USDT-P", symbol)  // "BTC/USDT-P" → "BTC/USDT-P/USDT-P"
}
```

✅ **Right:** Strip known suffixes before appending
```rust
fn parse_symbol(symbol: &str) -> String {
    let base = symbol.to_uppercase().trim_end_matches("/USDT-P").to_string();
    format!("{}/USDT-P", base)
}
```

### 5. **Global vs Exchange-Specific Symbols in Tests**
❌ **Wrong:** Use exchange-specific format in test calls
```rust
let ticker = client.get_ticker("BTC/USDT-P").await?;  // Brittle — format may change
```

✅ **Right:** Always use global format — the client handles conversion internally
```rust
let ticker = client.get_ticker("BTC").await?;
```

### 6. **Using `unwrap()` in Production Code**
❌ **Wrong:**
```rust
let value = parse_response.unwrap();  // PANIC on error!
```

✅ **Right:**
```rust
let value = parse_response.context("failed to parse response")?;
```

### 7. **Not Handling Rate Limits**
❌ **Wrong:** Fire `join_all` across 30+ symbols without rate limiting → instant IP ban
```rust
let futures = symbols.iter().map(|s| self.fetch_all_data(s));  // 90+ simultaneous requests
join_all(futures).await
```

✅ **Right:** Rate limiter inside `fetch_raw` throttles automatically; no extra work needed at call sites
```rust
// Rate limiter is held inside self.rate_limiter and called within fetch_raw/get
// join_all + rate_limiter = fan-out that auto-throttles
```

### 8. **Crossed or Unsorted Orderbook**
❌ **Wrong:** Return raw API levels without sorting
```rust
Ok(Orderbook { bids: raw_bids, asks: raw_asks, .. })  // Order not guaranteed
```

✅ **Right:** Always sort before returning
```rust
bids.sort_by(|a, b| b.price.cmp(&a.price));  // descending
asks.sort_by(|a, b| a.price.cmp(&b.price));  // ascending
```

### 9. **Liquidity Depth Showing All Zeros**
**Symptoms:** `cargo run -- liquidity` shows 0 for all bps levels

**Causes & fixes:**
- Orderbook is crossed → fix sort order
- `best_bid` or `best_ask` is zero → prices not parsed correctly
- Orderbook levels have quantity = 0 → check quantity field name in API response
- All levels are outside the 1–20 bps window → ensure depth is large enough

### 10. **Storing Wrong Historical Data**
If you discover a conversion bug after data has already been collected, write a corrective
SQL migration. Use the stored `last_price` column to recompute derived fields in-place:

```sql
UPDATE tickers
SET
    turnover_24h = volume_24h,
    volume_24h   = CASE WHEN last_price > 0 THEN volume_24h / last_price ELSE 0 END
WHERE exchange = '<exchange_name>'
  AND volume_24h IS NOT NULL AND volume_24h > 0;
```

### 11. **Using `f64` as Intermediate for Prices**

❌ **Wrong:** Convert exchange string prices via `f64` — loses precision on large prices
```rust
let price: f64 = s.parse().unwrap();
let decimal = Decimal::from_f64(price).unwrap();  // BTC at 95123.456789 may lose digits
```

✅ **Right:** Parse directly from string with `Decimal::from_str_exact`
```rust
let decimal = Decimal::from_str_exact(s).context("failed to parse price")?;
```

Use `Decimal::from_f64` **only** when the API returns a native `f64` JSON number with no string
alternative. In that case, document the precision loss in a comment.

### 12. **Kline `close_time` Left as `open_time`**

❌ **Wrong:** Set `close_time = open_time` when the API doesn't provide it
```rust
Ok(Kline { open_time, close_time: open_time, .. })  // close_time == open_time confuses queries
```

✅ **Right:** Approximate from the interval string
```rust
fn kline_close_time(open_time: DateTime<Utc>, interval: &str) -> DateTime<Utc> {
    let duration = match interval {
        "1m" => chrono::Duration::minutes(1),
        "5m" => chrono::Duration::minutes(5),
        "15m" => chrono::Duration::minutes(15),
        "1h" => chrono::Duration::hours(1),
        "4h" => chrono::Duration::hours(4),
        "1d" => chrono::Duration::days(1),
        _ => chrono::Duration::hours(1),
    };
    open_time + duration - chrono::Duration::milliseconds(1)
}
```

### 13. **Hardcoded `funding_interval`**

❌ **Wrong:** Hardcode the funding interval without verifying against the exchange
```rust
FundingRate { funding_interval: 8, .. }  // Assumes 8-hour funding — wrong for 1-hour exchanges
```

**Symptoms:** Annualized funding rate calculations in downstream analytics are 8× too high/low.

✅ **Right:** Read from API if available; otherwise document the assumption
```rust
// Hotstuff uses 1-hour funding; verified from exchange docs 2026-01-xx
FundingRate { funding_interval: 1, .. }

// Or read from API:
let interval = resp.funding_interval_hours.unwrap_or(8);
FundingRate { funding_interval: interval, .. }
```

### 14. **Including Delisted/Inactive Markets in Batch**

❌ **Wrong:** Fetch all markets including inactive ones
```rust
let symbols = self.get_markets().await?;  // Returns 200 markets, 50 are delisted
let futures = symbols.iter().map(|s| self.get_ticker(s));  // 50 fail with 404
```

**Symptoms:** Batch logs flooded with 404 errors; API budget wasted on dead symbols.

✅ **Right:** Filter to active/live markets only
```rust
let markets = self.get_markets().await?;
let symbols: Vec<_> = markets
    .iter()
    .filter(|m| m.status.as_deref() == Some("active") || m.is_live)
    .map(|m| m.symbol.clone())
    .collect();
```

Always check what status field the exchange provides for market lifecycle (`status`, `state`,
`is_active`, `tradable`, etc.) and filter accordingly.

### 15. **`price_change_24h` / `price_change_pct` Left as Zero**

Some exchanges provide `change_24h` as an absolute price change; others provide a percentage.
A common mistake is to leave both fields as `Decimal::ZERO` because the mapping isn't obvious.

**How to derive when not directly available:**
```rust
// If API gives open price:
let price_change_24h = last_price - open_price_24h;
let price_change_pct = if open_price_24h > Decimal::ZERO {
    price_change_24h / open_price_24h  // ratio, e.g., -0.012
} else {
    Decimal::ZERO
};

// If API gives absolute change only:
let price_change_24h = parse_decimal(&t.change_24h);
let price_change_pct = if last_price > Decimal::ZERO && price_change_24h != Decimal::ZERO {
    let prev = last_price - price_change_24h;
    if prev > Decimal::ZERO { price_change_24h / prev } else { Decimal::ZERO }
} else {
    Decimal::ZERO
};
```

Never store zeros for these when the data is available — they are displayed prominently in
the UI and used in screening.

### 16. **Trade Side Defaulting to `Buy` on Unknown**

❌ **Wrong:** Silently default unknown side to `Buy`
```rust
let side = match t.side.as_deref() {
    Some("b") => OrderSide::Buy,
    _ => OrderSide::Buy,  // Unknown side silently treated as Buy — skews buy/sell ratio
};
```

✅ **Right:** Log a warning for unknown values; only default as last resort
```rust
let side = match t.side.as_deref() {
    Some("b") | Some("buy") | Some("Buy") => OrderSide::Buy,
    Some("s") | Some("sell") | Some("Sell") => OrderSide::Sell,
    other => {
        tracing::warn!("Unknown trade side {:?} for {}, defaulting to Buy", other, symbol);
        OrderSide::Buy
    }
};
```

This helps catch new side formats added by the exchange (e.g., `"long"` / `"short"`).

### 17. **Mixing `contract` Symbol with `underlying` Symbol**

Some exchanges return two symbol fields per market:
- `symbol` = exchange contract name (e.g., `"BTC-USDT-PERP"`) — used in API calls
- `underlying_symbol` or `base_asset` = global base asset (e.g., `"BTC"`) — used in DB

❌ **Wrong:** Store the contract name as the global symbol
```rust
Market { symbol: contract.symbol.clone(), .. }  // "BTC-USDT-PERP" stored in DB → breaks joins
```

✅ **Right:** Store the underlying/base asset as the global symbol
```rust
Market { symbol: contract.underlying_symbol.clone(), .. }  // "BTC" stored in DB
```

The contract name belongs in `Market.contract`; the global symbol belongs in `Market.symbol`.

### 18. **`serde rename_all` Mismatch — All-Zero Silent Output**

> **QFEX retrospective (2026-03):** Applied `#[serde(rename_all = "camelCase")]` globally to
> `SymbolMetrics`. The API returns `current_mark_price` (snake_case) but serde looked for
> `currentMarkPrice`. Every `Option<f64>` field deserialized as `None`. Code then called
> `.unwrap_or(Decimal::ZERO)`, producing valid-looking zero values with no error or warning.
> The only clue was that timestamps were correct while all prices/volumes were zero.

❌ **Wrong:** Apply `rename_all` globally without verifying every endpoint uses that convention
```rust
#[serde(rename_all = "camelCase")]  // assumes ALL endpoints return camelCase
pub struct SymbolMetrics {
    pub current_mark_price: Option<f64>,  // serde looks for "currentMarkPrice" — not found → None
    pub volume_24h_usd_notional: Option<f64>,  // same — None
}
```

✅ **Right:** `curl` each endpoint before writing any serde struct. Different endpoints on the
same exchange often use different naming conventions. Use explicit `#[serde(rename = "...")]`
only for fields that deviate from the struct's convention.

```bash
# Always verify field names before writing types.rs:
curl -s 'https://api.exchange.com/symbols/metrics' | jq '.[0] | keys'
curl -s 'https://api.exchange.com/refdata' | jq '.[0] | keys'
curl -s 'https://api.exchange.com/candles?symbol=BTC&resolution=1HOUR' | jq '.[0] | keys'
```

```rust
// After verifying: metrics endpoint uses snake_case, candles endpoint uses camelCase
// Do NOT apply rename_all globally; annotate per-struct as needed.

/// API uses snake_case field names (verified by curl 2026-03).
#[derive(Debug, Clone, Deserialize)]
pub struct SymbolMetrics {
    pub current_mark_price: Option<f64>,
    pub volume_24h_usd_notional: Option<f64>,
}

/// API uses camelCase for these specific fields (verified by curl 2026-03).
#[derive(Debug, Clone, Deserialize)]
pub struct CandleEntry {
    #[serde(rename = "startedAt")]
    pub started_at: Option<String>,
    #[serde(rename = "usdVolume")]
    pub usd_volume: Option<f64>,
}
```

**Diagnosis tip:** If a ticker command shows `$0.00` for all prices/volumes but a valid
timestamp is returned, immediately suspect a serde naming mismatch — the timestamp field
parsed but numeric fields silently fell back to zero.

### 19. **Numeric Fields That May Be Empty Strings**

> **QFEX retrospective (2026-03):** The `/refdata` endpoint returned `"underlier_price": ""`
> (empty string, not `null`). Declaring the field as `Option<f64>` caused a hard serde
> deserialization error on every request, making `is_supported()` fail with a cryptic
> "failed to fetch /refdata" error.

❌ **Wrong:** Declare as `Option<f64>` when the API may return `""` for missing values
```rust
pub underlier_price: Option<f64>,  // Fails with serde error when value is ""
```

✅ **Right:** Declare as `Option<String>` and parse at conversion time
```rust
/// May be `""` (empty string) when not available — use `Option<String>`.
pub underlier_price: Option<String>,

// In conversions.rs:
let underlier_price = metrics.underlier_price
    .as_deref()
    .filter(|s| !s.is_empty())
    .and_then(|s| Decimal::from_str_exact(s).ok())
    .unwrap_or(Decimal::ZERO);
```

**How to detect:** When initial integration returns HTTP/serde errors on what should be a
simple list endpoint, print the raw response body before parsing:
```rust
let body = resp.text().await?;
tracing::debug!("Raw response: {}", body);  // Add temporarily during development
let parsed: Vec<RefDataItem> = serde_json::from_str(&body)
    .context("failed to parse refdata")?;
```

### 20. **Unrounded `Decimal` Division Producing 28-Digit Output**

> **QFEX retrospective (2026-03):** `turnover / mark_price` with Rust `Decimal` produced
> `14.793940319971238540355923063` — 28 significant figures stored in the database and
> displayed in the CLI.

❌ **Wrong:** Store derived volumes/notionals without rounding
```rust
let volume_24h = turnover_24h / mark_price;  // 14.793940319971238540355923063
```

✅ **Right:** Always `.round_dp(N)` after division operations on financial quantities
```rust
let volume_24h = (turnover_24h / mark_price).round_dp(6);  // 14.793940
let open_interest_notional = (open_interest * mark_price).round_dp(2);  // USD → 2 dp
```

**Rule of thumb:** volumes in base assets → round to 6 dp; USD notional values → round to 2 dp.

### 21. **Integration Tests That Don't Catch Zero-Value Bugs**

> **QFEX retrospective (2026-03):** All integration tests passed (`ticker.symbol == "BTC"`,
> `!tickers.is_empty()`) even though every numeric field was zero. A one-line value assertion
> would have caught the serde bug immediately.

❌ **Wrong:** Only assert structural properties — presence, symbol name, list not empty
```rust
let ticker = client.get_ticker("BTC").await.unwrap();
assert_eq!(ticker.symbol, "BTC");          // Passes even when price = $0
assert!(!tickers.is_empty());              // Passes even when all prices = $0
```

✅ **Right:** Assert that key numeric fields are non-zero
```rust
let ticker = client.get_ticker("BTC").await.unwrap();
assert_eq!(ticker.symbol, "BTC");
assert!(ticker.mark_price > Decimal::ZERO,
    "mark_price should be non-zero; got {}. Possible serde naming mismatch.", ticker.mark_price);
assert!(ticker.turnover_24h > Decimal::ZERO,
    "turnover_24h should be non-zero; got {}", ticker.turnover_24h);
// Sanity: turnover should be much larger than base volume for most assets
if ticker.volume_24h > Decimal::ZERO {
    assert!(ticker.turnover_24h > ticker.volume_24h,
        "turnover ({}) should exceed volume ({}) for USDT-margined perps",
        ticker.turnover_24h, ticker.volume_24h);
}
```

### 22. **`is_supported` Making a Raw HTTP Call**

> **QFEX retrospective (2026-03):** `is_supported` called `GET /refdata` directly, bypassing
> the `status == "ACTIVE"` filter already applied in `get_markets()`. It also cached nothing,
> so every call hit the network.

❌ **Wrong:** duplicate the markets endpoint with a one-off HTTP call
```rust
async fn is_supported(&self, symbol: &str) -> Result<bool> {
    let qfex_sym = self.parse_symbol(symbol);
    let resp: RefdataResponse = self.get("/refdata", &[]).await?;
    Ok(resp.data.iter().any(|item| item.symbol == qfex_sym))  // includes INACTIVE markets!
}
```

✅ **Right:** delegate to `get_markets()`, which already filters active markets
```rust
async fn is_supported(&self, symbol: &str) -> Result<bool> {
    let markets = self.get_markets().await?;
    Ok(markets.iter().any(|m| m.symbol == self.parse_symbol(symbol)))
}
```

This approach is correct by construction: any market returned by `get_markets()` is guaranteed
to be active, and the logic is defined in exactly one place.

---

## Support & References

### Core Trait Definition
- **File:** `crates/perps-core/src/traits.rs`
- **Types:** `crates/perps-core/src/types.rs`

### Existing Implementations (reference in this order)
- **Hibachi (Full Phase 3 + caching):** `crates/perps-exchanges/src/hibachi/` — best modern example
- **Gravity (Full Phase 3):** `crates/perps-exchanges/src/gravity/`
- **Binance (Phase 2 + WebSocket):** `crates/perps-exchanges/src/binance/`
- **Bybit (Phase 2):** `crates/perps-exchanges/src/bybit/`

### Database Layer
- **Repository:** `crates/perps-database/src/repository.rs`
- **Migrations:** `migrations/` directory
- **Timestamp Utilities:** `crates/perps-database/src/timestamp.rs`

### CLI Commands
- **Entry:** `src/commands/mod.rs`
- **Ticker:** `src/commands/ticker.rs`
- **Market:** `src/commands/market.rs`
- **Liquidity:** `src/commands/liquidity.rs`
- **Start:** `src/commands/start.rs`

### API Server
- **Entry:** `src/commands/serve/mod.rs`
- **Handlers:** `src/commands/serve/handlers/`
- **Frontend:** `src/api/index.html`

---

## Questions?

Refer to:
1. `CLAUDE.md` - Project overview and critical rules
2. `docs/architecture.md` - System architecture
3. Existing exchange implementations for patterns (prefer Hibachi as the most complete example)
