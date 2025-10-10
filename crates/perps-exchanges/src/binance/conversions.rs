use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::str::FromStr;

use super::error::BinanceError;

/// Convert Binance timestamp (milliseconds) to DateTime<Utc>
pub fn timestamp_to_datetime(timestamp_ms: i64) -> Result<DateTime<Utc>, BinanceError> {
    DateTime::from_timestamp_millis(timestamp_ms)
        .ok_or_else(|| BinanceError::ConversionError(format!("Invalid timestamp: {}", timestamp_ms)))
}

/// Convert string to Decimal
pub fn str_to_decimal(s: &str) -> Result<Decimal, BinanceError> {
    Decimal::from_str(s).map_err(|e| {
        BinanceError::ConversionError(format!("Failed to parse decimal '{}': {}", s, e))
    })
}

/// Convert f64 to Decimal
#[allow(dead_code)]
pub fn f64_to_decimal(value: f64) -> Result<Decimal, BinanceError> {
    Decimal::try_from(value)
        .map_err(|e| BinanceError::ConversionError(format!("Failed to convert f64: {}", e)))
}

/// Normalize Binance symbol to our standard format (e.g., "BTC-USDT").
/// This function is idempotent.
pub fn normalize_symbol(binance_symbol: &str) -> String {
    // If it already contains a hyphen, assume it's already normalized.
    if binance_symbol.contains('-') {
        return binance_symbol.to_string();
    }
    // If it ends with USDT, add a hyphen.
    if let Some(base) = binance_symbol.strip_suffix("USDT") {
        format!("{}-USDT", base)
    } else {
        binance_symbol.to_string()
    }
}

/// Denormalize our symbol format back to Binance format
/// Example: "BTC-USDT" -> "BTCUSDT"
pub fn denormalize_symbol(symbol: &str) -> String {
    symbol.replace('-', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_symbol() {
        assert_eq!(normalize_symbol("BTCUSDT"), "BTC-USDT");
        assert_eq!(normalize_symbol("ETHUSDT"), "ETH-USDT");
        // Test for idempotency
        assert_eq!(normalize_symbol("BTC-USDT"), "BTC-USDT");
    }

    #[test]
    fn test_denormalize_symbol() {
        assert_eq!(denormalize_symbol("BTC-USDT"), "BTCUSDT");
        assert_eq!(denormalize_symbol("ETH-USDT"), "ETHUSDT");
    }

    #[test]
    fn test_str_to_decimal() {
        let result = str_to_decimal("123.45").unwrap();
        assert_eq!(result.to_string(), "123.45");
    }
}
