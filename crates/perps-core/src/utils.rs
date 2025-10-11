/// Utility functions for common operations across the perps-stats project

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
}
