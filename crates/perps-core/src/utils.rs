/// Utility functions for common operations across the perps-stats project

/// Normalize kline interval to a standard global format.
///
/// This function converts exchange-specific interval formats to a standard format:
/// - Standard format: 1m, 3m, 5m, 15m, 30m, 1h, 2h, 4h, 8h, 12h, 1d, 1w, 1M
/// - From KuCoin: 1min -> 1m, 1hour -> 1h, 1day -> 1d, 1week -> 1w
/// - From other formats: hour -> h, minute -> m, day -> d, week -> w
///
/// The global format is used for database storage to maintain consistency across exchanges.
pub fn normalize_interval(interval: &str) -> String {
    let lower = interval.to_lowercase();

    // Map common patterns to standard format
    match lower.as_str() {
        // Minutes
        "1min" | "1minute" => "1m".to_string(),
        "3min" | "3minutes" => "3m".to_string(),
        "5min" | "5minutes" => "5m".to_string(),
        "15min" | "15minutes" => "15m".to_string(),
        "30min" | "30minutes" => "30m".to_string(),

        // Hours
        "1hour" | "1hr" => "1h".to_string(),
        "2hour" | "2hr" | "2hours" => "2h".to_string(),
        "4hour" | "4hr" | "4hours" => "4h".to_string(),
        "6hour" | "6hr" | "6hours" => "6h".to_string(),
        "8hour" | "8hr" | "8hours" => "8h".to_string(),
        "12hour" | "12hr" | "12hours" => "12h".to_string(),

        // Days
        "1day" | "1d" => "1d".to_string(),

        // Weeks
        "1week" | "1w" => "1w".to_string(),

        // Months
        "1month" | "1mon" => "1M".to_string(),

        // If already in standard format, return as-is
        _ => {
            // Check if it's already in standard format (e.g., "1m", "1h", "1d", "1w")
            if interval.len() >= 2 {
                let last_char = interval.chars().last().unwrap_or(' ');
                if matches!(last_char, 'm' | 'h' | 'd' | 'w' | 'M') {
                    return interval.to_string();
                }
            }

            // Default: return as-is
            interval.to_string()
        }
    }
}

/// Extract the base currency symbol from an exchange-specific format to a global symbol.
///
/// This function extracts the base currency from exchange-specific symbol formats:
/// - "BTCUSDT" -> "BTC"
/// - "ETHUSDT" -> "ETH"
/// - "BTC-USD-PERP" -> "BTC"
/// - "XBTUSDTM" -> "BTC" (KuCoin's XBT is Bitcoin)
///
/// The global format is used for database storage to maintain consistency across exchanges.
pub fn extract_base_symbol(symbol: &str) -> String {
    let upper_symbol = symbol.to_uppercase();

    // Handle hyphenated formats first (e.g., "BTC-USD-PERP" -> "BTC")
    if upper_symbol.contains('-') {
        if let Some(base) = upper_symbol.split('-').next() {
            // KuCoin uses XBT for Bitcoin
            if base == "XBT" {
                return "BTC".to_string();
            }
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }

    // KuCoin uses XBT for Bitcoin and has 'M' suffix
    if upper_symbol.starts_with("XBT") {
        return "BTC".to_string();
    }

    // Common quote currencies to strip (order matters - longer first)
    let quote_currencies = ["USDTM", "USDT", "USDC", "BUSD", "TUSD", "USD"];

    // Try to strip quote currencies
    for quote in &quote_currencies {
        if let Some(base) = upper_symbol.strip_suffix(quote) {
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }

    // Default: return the symbol as-is (uppercase)
    upper_symbol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_base_symbol_binance() {
        assert_eq!(extract_base_symbol("BTCUSDT"), "BTC");
        assert_eq!(extract_base_symbol("ETHUSDT"), "ETH");
        assert_eq!(extract_base_symbol("btcusdt"), "BTC"); // Case insensitive
    }

    #[test]
    fn test_extract_base_symbol_kucoin() {
        assert_eq!(extract_base_symbol("XBTUSDTM"), "BTC"); // KuCoin XBT -> BTC
        assert_eq!(extract_base_symbol("ETHUSDTM"), "ETH");
    }

    #[test]
    fn test_extract_base_symbol_paradex() {
        assert_eq!(extract_base_symbol("BTC-USD-PERP"), "BTC");
        assert_eq!(extract_base_symbol("ETH-USD-PERP"), "ETH");
    }

    #[test]
    fn test_extract_base_symbol_lighter() {
        // Lighter already uses simple symbols
        assert_eq!(extract_base_symbol("BTC"), "BTC");
        assert_eq!(extract_base_symbol("ETH"), "ETH");
    }

    #[test]
    fn test_extract_base_symbol_bybit() {
        assert_eq!(extract_base_symbol("BTCUSDT"), "BTC");
        assert_eq!(extract_base_symbol("ETHUSDT"), "ETH");
    }

    #[test]
    fn test_extract_base_symbol_hyperliquid() {
        // Hyperliquid uses simple symbols
        assert_eq!(extract_base_symbol("BTC"), "BTC");
        assert_eq!(extract_base_symbol("ETH"), "ETH");
    }

    #[test]
    fn test_normalize_interval_minutes() {
        assert_eq!(normalize_interval("1min"), "1m");
        assert_eq!(normalize_interval("3min"), "3m");
        assert_eq!(normalize_interval("5min"), "5m");
        assert_eq!(normalize_interval("15min"), "15m");
        assert_eq!(normalize_interval("30min"), "30m");
        assert_eq!(normalize_interval("1minute"), "1m");
        assert_eq!(normalize_interval("5minutes"), "5m");
    }

    #[test]
    fn test_normalize_interval_hours() {
        assert_eq!(normalize_interval("1hour"), "1h");
        assert_eq!(normalize_interval("2hour"), "2h");
        assert_eq!(normalize_interval("4hour"), "4h");
        assert_eq!(normalize_interval("8hour"), "8h");
        assert_eq!(normalize_interval("12hour"), "12h");
        assert_eq!(normalize_interval("1hr"), "1h");
        assert_eq!(normalize_interval("4hours"), "4h");
    }

    #[test]
    fn test_normalize_interval_days_weeks() {
        assert_eq!(normalize_interval("1day"), "1d");
        assert_eq!(normalize_interval("1week"), "1w");
        assert_eq!(normalize_interval("1month"), "1M");
    }

    #[test]
    fn test_normalize_interval_already_standard() {
        // Already in standard format - should return as-is
        assert_eq!(normalize_interval("1m"), "1m");
        assert_eq!(normalize_interval("5m"), "5m");
        assert_eq!(normalize_interval("1h"), "1h");
        assert_eq!(normalize_interval("4h"), "4h");
        assert_eq!(normalize_interval("1d"), "1d");
        assert_eq!(normalize_interval("1w"), "1w");
        assert_eq!(normalize_interval("1M"), "1M");
    }

    #[test]
    fn test_normalize_interval_case_insensitive() {
        assert_eq!(normalize_interval("1MIN"), "1m");
        assert_eq!(normalize_interval("1HOUR"), "1h");
        assert_eq!(normalize_interval("1DAY"), "1d");
    }
}
