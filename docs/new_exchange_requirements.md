# Adding a New Perpetual DEX Exchange

This document describes all the requirements and checklist for integrating a new perpetual futures DEX into the perps-stats system.

## Overview

When adding a new exchange, you must ensure:
1. **Trait Implementation**: Full `IPerps` implementation (REST API is mandatory; WebSocket is optional)
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
    async fn get_orderbook(&self, symbol: &str, depth: u16) -> anyhow::Result<OrderBook> { /* ... */ }
}
```

**Extended Methods (Phase 3 - History & Analytics):**
```rust
    async fn get_funding_rate(&self, symbol: &str) -> anyhow::Result<FundingRate> { /* ... */ }
    async fn get_funding_rate_history(
        &self, symbol: &str, from: Option<i64>, to: Option<i64>, limit: Option<usize>
    ) -> anyhow::Result<Vec<FundingRateHistory>> { /* ... */ }

    async fn get_open_interest(&self, symbol: &str) -> anyhow::Result<OpenInterest> { /* ... */ }

    async fn get_klines(
        &self, symbol: &str, interval: &str, from: Option<i64>, to: Option<i64>, limit: Option<usize>
    ) -> anyhow::Result<Vec<Kline>> { /* ... */ }

    async fn get_recent_trades(&self, symbol: &str, limit: Option<usize>) -> anyhow::Result<Vec<Trade>> { /* ... */ }

    async fn get_market_stats(&self, symbol: &str) -> anyhow::Result<MarketStats> { /* ... */ }
    async fn get_all_market_stats(&self) -> anyhow::Result<Vec<MarketStats>> { /* ... */ }
}
```

**WebSocket Trait (Optional):**
If WebSocket streaming is supported, also implement `IPerpsStream`:
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

**Requirement:** Implement `parse_symbol()` to convert global symbols to exchange-specific format.

**Location:** `crates/perps-exchanges/src/<exchange_name>/client.rs`

```rust
impl ExchangeClient {
    /// Parse global symbol (e.g., "BTC") to exchange-specific format (e.g., "BTC_USDT_Perp")
    fn parse_symbol(&self, symbol: &str) -> String {
        // Example: BTC → BTC_USDT_Perp
        format!("{}_{}", symbol, "USDT_Perp")
    }

    /// Reverse parse: exchange format back to global (used in some contexts)
    fn reverse_parse_symbol(&self, exchange_symbol: &str) -> String {
        // Example: BTC_USDT_Perp → BTC
        exchange_symbol.split('_').next().unwrap_or("").to_string()
    }
}
```

### 1.4 Rate Limiting

**Requirement:** Implement client-side rate limiting to respect exchange limits.

**Location:** `crates/perps-exchanges/src/<exchange_name>/client.rs`

```rust
use perps_core::rate_limiter::RateLimiter;

pub struct ExchangeClient {
    base_url: String,
    rate_limiter: RateLimiter,
    // ... other fields
}

impl ExchangeClient {
    pub async fn new() -> anyhow::Result<Self> {
        // Example: 50 requests per second
        let rate_limiter = RateLimiter::new(50, std::time::Duration::from_secs(1));
        Ok(Self {
            base_url: "https://api.example.com".to_string(),
            rate_limiter,
            // ...
        })
    }

    async fn make_request(&self, path: &str) -> anyhow::Result<String> {
        // Wait for rate limit token before making request
        self.rate_limiter.acquire().await;

        let response = reqwest::Client::new()
            .get(&format!("{}{}", self.base_url, path))
            .send()
            .await?;

        Ok(response.text().await?)
    }
}
```

### 1.5 Error Handling & Logging

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

    Ok(ticker.into()) // Use From/Into for type conversion
}
```

---

## Phase 2: Factory Registration

### 2.1 Register in Factory

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
    // ... async clients
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

### 2.2 Add Factory Tests

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

## Phase 3: Database Integration

### 3.1 Create Migration

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

### 3.2 Run Migration

```bash
# Verify migration is recognized
cargo sqlx prepare --database-url $DATABASE_URL

# Run migration
cargo run -- db migrate
```

---

## Phase 4: CLI Command Integration

All major CLI commands must work with the new exchange. Verify these commands:

### 4.1 Ticker Command

**Test:**
```bash
cargo run -- ticker --exchange <exchange_name> --symbols BTC,ETH
```

**Expected:** Display current ticker prices

**Code Location:** `src/commands/ticker.rs`
- Exchange is already dynamically loaded via `factory.rs`
- No changes needed if `IPerps` is properly implemented

### 4.2 Market Command

**Test:**
```bash
cargo run -- market --exchange <exchange_name> --symbols BTC
```

**Expected:** Display comprehensive market data (price, volume, funding rate, etc.)

**Code Location:** `src/commands/market.rs`
- Calls `get_market_stats()` method
- Ensure method is properly implemented in `IPerps`

### 4.3 Liquidity Command

**Test:**
```bash
cargo run -- liquidity --exchange <exchange_name> --symbols BTC,ETH
```

**Expected:** Display liquidity depth at various spreads (1, 2.5, 5, 10, 20 bps)

**Code Location:** `src/commands/liquidity.rs`
- Calls `get_orderbook()` method
- Aggregator calculates depth from orderbook
- Ensure orderbook format is correct

### 4.4 Start Command

**Test:**
```bash
cargo run -- start --exchanges <exchange_name> --symbols BTC,ETH --report-interval 60
```

**Expected:** Periodically fetch and store tickers and liquidity data

**Code Location:** `src/commands/start.rs`
- Exchanges are loaded from factory
- Uses batch collection system
- Stores data via `repository.store_tickers_with_exchange()`

### 4.5 Serve Command (API Server)

**Test:**
```bash
cargo run -- serve --port 8080
curl http://127.0.0.1:8080/api/v1/exchanges
```

**Expected:** API returns exchange list including new exchange from database

**Code Location:** `src/commands/serve/handlers/exchanges.rs`
- Queries database directly: `SELECT * FROM exchanges`
- No code changes needed (database-driven)

---

## Phase 5: API Frontend Update

### 5.1 Frontend Dynamic Loading

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
- Once migration 07+ runs, new exchange appears in database
- API endpoint returns new exchange
- Frontend automatically displays it (dynamic loading)

---

## Phase 6: Testing

### 6.1 Unit Tests

**Location:** `crates/perps-exchanges/src/<exchange_name>/` or `tests/`

**Minimum Coverage:**
- Symbol parsing: `test_parse_symbol_*`
- Type conversions: `test_ticker_conversion`, `test_orderbook_conversion`
- Error handling: `test_invalid_symbol`, `test_api_error`

**Example:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_symbol_btc() {
        let client = ExchangeClient::new().unwrap();
        assert_eq!(client.parse_symbol("BTC"), "BTC_USDT_Perp");
    }

    #[tokio::test]
    async fn test_get_markets_returns_vec() {
        let client = ExchangeClient::new().unwrap();
        let markets = client.get_markets().await;
        assert!(markets.is_ok());
        assert!(!markets.unwrap().is_empty());
    }
}
```

### 6.2 Integration Tests

**Location:** `crates/perps-exchanges/tests/<exchange_name>_integration_tests.rs`

**Pattern:**
```rust
#[tokio::test]
async fn test_get_ticker_integration() {
    let client = ExchangeClient::new().unwrap();
    let ticker = client.get_ticker("BTC").await;

    // Gracefully handle API unavailability
    match ticker {
        Ok(t) => {
            assert_eq!(t.symbol, "BTC");
            assert!(t.price > Decimal::ZERO);
        }
        Err(e) => {
            eprintln!("API unavailable or symbol not found: {}", e);
            // Test passes if API is unavailable
        }
    }
}
```

### 6.3 Full Integration Test Checklist

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
- [ ] `parse_symbol()` implemented for symbol mapping
- [ ] Rate limiting implemented
- [ ] Error handling uses `.context()` and `anyhow::Result`
- [ ] Logging uses `tracing` macros (`debug!`, `info!`, `warn!`, `error!`)
- [ ] Public functions have doc comments

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
- [ ] `cargo run -- liquidity --exchange <exchange_name> --symbols BTC` works
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
- [ ] Unit tests for symbol parsing and conversions
- [ ] Integration tests for all `IPerps` methods
- [ ] Factory tests added
- [ ] All tests pass: `cargo test`
- [ ] Clippy passes: `cargo clippy`
- [ ] Format passes: `cargo fmt --check`

### Documentation
- [ ] README updated with new exchange in supported exchanges table
- [ ] Known limitations documented

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

### 2. **Not Implementing All 12 Methods**
❌ **Wrong:** Leave methods returning `Err(anyhow::anyhow!("not implemented"))`

✅ **Right:** Implement all 12 methods or gracefully handle unsupported features
```rust
async fn get_funding_rate_history(&self, symbol: &str, from: Option<i64>, to: Option<i64>, limit: Option<usize>) -> anyhow::Result<Vec<FundingRateHistory>> {
    // If API doesn't support history, return current rate as single element
    let current = self.get_funding_rate(symbol).await?;
    Ok(vec![FundingRateHistory {
        symbol: symbol.to_string(),
        funding_rate: current.funding_rate,
        timestamp: Utc::now(),
    }])
}
```

### 3. **Global vs Exchange-Specific Symbols**
❌ **Wrong:** Use exchange-specific format directly
```rust
let ticker = client.get_ticker("BTCUSDT").await?;  // Brittle!
```

✅ **Right:** Accept global symbol and convert internally
```rust
let ticker = client.get_ticker("BTC").await?;  // Client converts to BTCUSDT
```

### 4. **Using `unwrap()` in Production Code**
❌ **Wrong:**
```rust
let value = parse_response.unwrap();  // PANIC on error!
```

✅ **Right:**
```rust
let value = parse_response.context("failed to parse response")?;
```

### 5. **Hardcoding API Base URL**
❌ **Wrong:**
```rust
let url = "https://api.example.com/ticker".to_string();  // Can't change without recompile
```

✅ **Right:**
```rust
pub struct ExchangeClient {
    base_url: String,  // Can be injected or configured
}
```

### 6. **Not Handling Rate Limits**
❌ **Wrong:** Fire requests without rate limiting
✅ **Right:** Use `RateLimiter` to queue requests

### 7. **Missing Type Conversions**
❌ **Wrong:** Return API response types directly
✅ **Right:** Convert to `perps_core` domain types in `conversions.rs`

---

## Support & References

### Core Trait Definition
- **File:** `crates/perps-core/src/traits.rs`
- **Types:** `crates/perps-core/src/types.rs`

### Existing Implementations
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
3. Existing exchange implementations for patterns

