# Grafana Local Setup Guide

Quick guide to run Grafana locally (without Docker) to visualize perps-stats data.

## Prerequisites

- Grafana installed via Homebrew: `brew install grafana`
- PostgreSQL running with `perps_stats` database
- Data collection running (via `cargo run -- start`)

## Quick Start

### 1. Start Grafana

```bash
./scripts/start_grafana.sh
```

This will:
- Start Grafana on http://localhost:3000
- Auto-configure PostgreSQL datasource (perps_stats database)
- Load pre-built dashboards

### 2. Login

- **URL**: http://localhost:3000
- **Username**: `admin`
- **Password**: `admin`

You'll be prompted to change the password on first login (optional for local dev).

### 3. Access Dashboards

Navigate to: **Dashboards → Perps Stats** folder

Available dashboards:
1. **Simple Test Dashboard** - Basic overview with BTC data
2. **Exchange Performance Dashboard** - Comprehensive comparison
3. **Liquidity Analysis Dashboard** - Deep dive into liquidity

## Manual Setup (Alternative)

If the startup script doesn't work, you can start Grafana manually:

### 1. Create directories

```bash
mkdir -p grafana/data grafana/logs grafana/plugins
```

### 2. Start Grafana

```bash
grafana-server \
  --homepath=/opt/homebrew/opt/grafana/share/grafana \
  --config=grafana/local.ini
```

### 3. Configure Datasource Manually

1. Go to **Configuration → Data Sources → Add data source**
2. Select **PostgreSQL**
3. Configure:
   - **Name**: `perps-stats`
   - **Host**: `localhost:5432`
   - **Database**: `perps_stats`
   - **User**: `perps` (or your PostgreSQL username)
   - **Password**: (leave empty if using trust auth)
   - **SSL Mode**: `disable`
   - **TimescaleDB**: Enable
4. Click **Save & Test** - should show "Database Connection OK"

### 4. Import Dashboards

1. Go to **Dashboards → Import**
2. Upload JSON files from `grafana/dashboards/`:
   - `simple_test.json`
   - `exchange_performance.json`
   - `liquidity_analysis.json`

## Troubleshooting

### Connection refused / Cannot connect to database

**Check PostgreSQL is running**:
```bash
psql postgres://localhost/perps_stats -c "SELECT 1;"
```

**Check database exists**:
```bash
psql -l | grep perps_stats
```

**Check your PostgreSQL user**:
```bash
whoami  # This is usually your PostgreSQL username
```

Update datasource configuration if needed:
```bash
# Edit grafana/provisioning/datasources/local_datasources.yml
# Set user to your PostgreSQL username
```

### No data in panels

**Check data exists**:
```bash
psql postgres://localhost/perps_stats -c "
  SELECT e.name, COUNT(*), MAX(t.ts)
  FROM tickers t
  JOIN exchanges e ON t.exchange_id = e.id
  GROUP BY e.name;
"
```

**Adjust time range**:
- Click time picker (top right)
- Select "Last 24 hours" or "Last 7 days"
- Click "Apply"

**Check symbol exists**:
- Most queries default to BTC
- Verify BTC data exists: `SELECT COUNT(*) FROM tickers WHERE symbol = 'BTC';`

### Datasource not appearing in panels

**Restart Grafana**:
```bash
# Stop Grafana (Ctrl+C)
# Start again
./scripts/start_grafana.sh
```

**Set datasource as default**:
1. Go to **Configuration → Data Sources**
2. Click on `perps-stats`
3. Check "Default" checkbox
4. Click "Save & Test"

## Configuration Files

| File | Purpose |
|------|---------|
| `grafana/local.ini` | Grafana server configuration |
| `grafana/provisioning/datasources/local_datasources.yml` | Auto-configure PostgreSQL connection |
| `grafana/provisioning/dashboards/dashboards.yml` | Auto-load dashboards |
| `grafana/dashboards/*.json` | Pre-built dashboards |
| `scripts/start_grafana.sh` | Startup script |

## Testing the Setup

### 1. Verify Datasource

Go to **Configuration → Data Sources → perps-stats → Settings**

Scroll down and click **Save & Test**

Expected: ✅ "Database Connection OK"

### 2. Test Simple Query

In datasource settings, go to **Explore** tab or click **Explore** button.

Run this query:
```sql
SELECT
  e.name AS exchange,
  COUNT(*) as count
FROM tickers t
JOIN exchanges e ON t.exchange_id = e.id
GROUP BY e.name;
```

Expected: Table showing exchange names and counts

### 3. View Test Dashboard

Navigate to: **Dashboards → Perps Stats → Simple Test Dashboard**

Expected panels:
1. **Current Data Summary** - Table with all exchanges and symbols
2. **BTC Price Comparison** - Line chart comparing prices
3. **BTC Bid-Ask Spread** - Spread over time
4. **BTC Liquidity Depth** - Liquidity at 1 bps
5. **BTC Slippage** - Slippage for $100K trades

## Next Steps

### Customize Dashboards

1. Click dashboard title → **Settings**
2. Add variables (symbol, exchange)
3. Clone panels and modify queries
4. Save changes

### Create New Panels

1. Click **Add panel** button
2. Select visualization type
3. Copy query from `docs/GRAFANA_QUERIES_TESTED.sql`
4. Adjust time range and refresh
5. Click **Apply**

### Explore More Queries

See `grafana/QUERY_INDEX.md` for:
- 25 production-ready queries
- Query selection guide by use case
- Panel type recommendations
- Common patterns and examples

## Resources

- **Complete Query Library**: `docs/GRAFANA_QUERIES_TESTED.sql`
- **Query Index**: `grafana/QUERY_INDEX.md`
- **Database Schema**: `migrations/00_initial_schema.sql`
- **Grafana Docs**: https://grafana.com/docs/

---

**Quick Commands Reference**

```bash
# Start Grafana
./scripts/start_grafana.sh

# Check data
psql postgres://localhost/perps_stats -c "SELECT e.name, COUNT(*) FROM tickers t JOIN exchanges e ON t.exchange_id = e.id GROUP BY e.name;"

# Check logs
tail -f grafana/logs/grafana.log

# Stop Grafana
# Press Ctrl+C in terminal
```
