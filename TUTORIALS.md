# `perps-stats` CLI Tutorials

This document provides a comprehensive guide to using the `perps-stats` command-line interface (CLI). Each section details a specific command, its purpose, available options, and practical examples.

## Global Options

These options are available for all commands:

-   `--help`: Displays a help message for the command.
-   `--version`: Shows the current version of the `perps-stats` application.

## Commands


### `market`

Retrieve L1 market data for contracts.

**Usage:**

```bash
cargo run -- market [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

-   `-e, --exchange <EXCHANGE>`: The exchange to query. Defaults to `binance`.
-   `-s, --symbols <SYMBOLS>`: A comma-separated list of symbols.
-   `-f, --format <FORMAT>`: The output format (`table`, `json`). Defaults to `table`.
-   `-d, --detailed`: Shows detailed information, including order book depth.
-   `-t, --timeframe <TIMEFRAME>`: The timeframe for statistics (`5m`, `15m`, `1h`, `24h`). Defaults to `24h`.

**Examples:**

-   **Get market data for BTC from Binance:**

    ```bash
    cargo run -- market -s BTC
    ```

-   **Get detailed ETH data from Bybit in JSON format:**

    ```bash
    cargo run -- market --exchange bybit -s ETH --detailed --format json
    ```

### `liquidity`

Retrieve liquidity depth for contracts.

**Usage:**

```bash
cargo run -- liquidity [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

-   `-e, --exchange <EXCHANGE>`: The exchange to query.
-   `-s, --symbols <SYMBOLS>`: A comma-separated list of symbols.
-   `-f, --format <FORMAT>`: The output format (`table`, `json`, `csv`). Defaults to `table`.
-   `-o, --output <OUTPUT>`: The output file path for CSV format.
-   `-d, --output-dir <OUTPUT_DIR>`: The output directory for CSV files.
-   `-i, --interval <INTERVAL>`: The fetch interval in seconds for periodic fetching.
-   `-n, --max-snapshots <MAX_SNAPSHOTS>`: The maximum number of snapshots to collect (0 for unlimited).

**Examples:**

-   **Get liquidity data for BTC and ETH from Binance:**

    ```bash
    cargo run -- liquidity --exchange binance -s BTC,ETH
    ```

-   **Save liquidity data to a CSV file:**

    ```bash
    cargo run -- liquidity -s BTC,ETH --format csv --output liquidity_data.csv
    ```

### `ticker`

Retrieve ticker data for contracts.

**Usage:**

```bash
cargo run -- ticker [OPTIONS] --symbols <SYMBOLS>
```

**Arguments:**

-   `-e, --exchange <EXCHANGE>`: The exchange to query. Defaults to `binance`.
-   `-s, --symbols <SYMBOLS>`: A comma-separated list of symbols.
-   `-f, --format <FORMAT>`: The output format (`table`, `json`, `csv`, `excel`). Defaults to `table`.
-   `-o, --output <OUTPUT>`: The output file path for CSV/Excel format.
-   `-d, --output-dir <OUTPUT_DIR>`: The output directory for files.
-   `-i, --interval <INTERVAL>`: The fetch interval in seconds.
-   `-n, --max-snapshots <MAX_SNAPSHOTS>`: The maximum number of snapshots to collect.

**Examples:**

-   **Get ticker data for BTC and ETH from Binance:**

    ```bash
    cargo run -- ticker -s BTC,ETH
    ```

-   **Save ticker data to an Excel file:**

    ```bash
    cargo run -- ticker -s BTC,ETH --format excel --output ticker_data.xlsx
    ```
