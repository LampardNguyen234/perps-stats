use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use crate::commands::serve::middleware::error::AppError;

/// Query parameters for time-range based endpoints
#[derive(Debug, Deserialize)]
pub struct TimeRangeQuery {
    /// Exchange name filter (e.g., "binance", "bybit") - optional
    pub exchange: Option<String>,

    /// Symbol (required, e.g., "BTC", "ETH")
    pub symbol: String,

    /// Start timestamp (Unix timestamp in seconds)
    #[serde(default)]
    pub start: Option<i64>,

    /// End timestamp (Unix timestamp in seconds)
    #[serde(default)]
    pub end: Option<i64>,

    /// Maximum number of records to return (default: 100, max: 500)
    #[serde(default = "default_limit")]
    pub limit: i64,

    /// Pagination offset
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    100
}

impl TimeRangeQuery {
    /// Get symbol normalized to uppercase
    pub fn normalized_symbol(&self) -> String {
        self.symbol.to_uppercase()
    }

    /// Validate required parameters
    pub fn validate(&self) -> Result<(), AppError> {
        // Validate symbol is not empty
        if self.symbol.trim().is_empty() {
            return Err(AppError::bad_request("Symbol parameter is required and cannot be empty".to_string()));
        }

        // Validate symbol format (alphanumeric, max 20 chars) - case insensitive
        if !self.symbol.chars().all(|c| c.is_alphanumeric()) || self.symbol.len() > 20 {
            return Err(AppError::bad_request("Symbol must be alphanumeric and max 20 characters".to_string()));
        }

        // Validate exchange if provided
        if let Some(exchange) = &self.exchange {
            if exchange.trim().is_empty() {
                return Err(AppError::bad_request("Exchange parameter cannot be empty".to_string()));
            }
            if !exchange.chars().all(|c| c.is_alphanumeric() || c == '_') || exchange.len() > 50 {
                return Err(AppError::bad_request("Exchange must be alphanumeric (with underscores) and max 50 characters".to_string()));
            }
        }

        // Validate limit
        if self.limit < 1 || self.limit > 1000 {
            return Err(AppError::bad_request("Limit must be between 1 and 1000".to_string()));
        }

        // Validate offset
        if self.offset < 0 {
            return Err(AppError::bad_request("Offset must be non-negative".to_string()));
        }

        // Validate timestamp range
        if let (Some(start), Some(end)) = (self.start, self.end) {
            if start > end {
                return Err(AppError::bad_request("Start timestamp must be before end timestamp".to_string()));
            }
            // Check for reasonable timestamp ranges (not too far in the past/future)
            let now = Utc::now().timestamp();
            let one_year_ago = now - (365 * 24 * 3600); // 1 year ago
            let one_year_future = now + (365 * 24 * 3600); // 1 year in future
            
            if start < one_year_ago || start > one_year_future {
                return Err(AppError::bad_request("Start timestamp is outside reasonable range".to_string()));
            }
            if end < one_year_ago || end > one_year_future {
                return Err(AppError::bad_request("End timestamp is outside reasonable range".to_string()));
            }
        }

        Ok(())
    }

    /// Validate and cap the limit to maximum allowed
    pub fn validated_limit(&self) -> i64 {
        self.limit.min(500).max(1)
    }

    /// Convert Unix timestamp to DateTime<Utc>
    pub fn start_datetime(&self) -> DateTime<Utc> {
        self.start
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
            .unwrap_or_else(|| Utc::now() - chrono::Duration::hours(24))
    }

    /// Convert Unix timestamp to DateTime<Utc>
    pub fn end_datetime(&self) -> DateTime<Utc> {
        self.end
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
            .unwrap_or_else(Utc::now)
    }
}

/// Query parameters for kline (OHLCV) endpoints
#[derive(Debug, Deserialize)]
pub struct KlineQuery {
    /// Exchange name (required)
    pub exchange: String,

    /// Symbol (required)
    pub symbol: String,

    /// Interval (e.g., "1m", "5m", "1h", "1d")
    pub interval: String,

    /// Start timestamp (Unix timestamp in seconds)
    pub start: Option<i64>,

    /// End timestamp (Unix timestamp in seconds)
    pub end: Option<i64>,

    /// Maximum number of records (default: 100, max: 500)
    #[serde(default = "default_limit")]
    pub limit: i64,
}

impl KlineQuery {
    /// Validate required parameters
    pub fn validate(&self) -> Result<(), AppError> {
        // Validate exchange is not empty
        if self.exchange.trim().is_empty() {
            return Err(AppError::bad_request("Exchange parameter is required and cannot be empty".to_string()));
        }

        // Validate exchange format
        if !self.exchange.chars().all(|c| c.is_alphanumeric() || c == '_') || self.exchange.len() > 50 {
            return Err(AppError::bad_request("Exchange must be alphanumeric (with underscores) and max 50 characters".to_string()));
        }

        // Validate symbol is not empty
        if self.symbol.trim().is_empty() {
            return Err(AppError::bad_request("Symbol parameter is required and cannot be empty".to_string()));
        }

        // Validate symbol format
        if !self.symbol.chars().all(|c| c.is_alphanumeric()) || self.symbol.len() > 20 {
            return Err(AppError::bad_request("Symbol must be alphanumeric and max 20 characters".to_string()));
        }

        // Validate interval format (basic validation)
        if self.interval.trim().is_empty() {
            return Err(AppError::bad_request("Interval parameter is required and cannot be empty".to_string()));
        }

        // Basic interval format check (1m, 5m, 1h, 1d, etc.)
        let valid_interval = self.interval.chars().enumerate().all(|(i, c)| {
            if i == self.interval.len() - 1 {
                // Last character should be a unit (m, h, d, w)
                matches!(c, 'm' | 'h' | 'd' | 'w')
            } else {
                // Other characters should be digits
                c.is_ascii_digit()
            }
        });

        if !valid_interval || self.interval.len() > 5 {
            return Err(AppError::bad_request("Invalid interval format. Use format like '1m', '5m', '1h', '1d'".to_string()));
        }

        // Validate limit
        if self.limit < 1 || self.limit > 1000 {
            return Err(AppError::bad_request("Limit must be between 1 and 1000".to_string()));
        }

        Ok(())
    }

    /// Validate and cap the limit to maximum allowed
    pub fn validated_limit(&self) -> i64 {
        self.limit.min(500).max(1)
    }

    /// Convert Unix timestamp to DateTime<Utc>
    pub fn start_datetime(&self) -> DateTime<Utc> {
        self.start
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
            .unwrap_or_else(|| Utc::now() - chrono::Duration::hours(24))
    }

    /// Convert Unix timestamp to DateTime<Utc>
    pub fn end_datetime(&self) -> DateTime<Utc> {
        self.end
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
            .unwrap_or_else(Utc::now)
    }
}

/// Query parameters for simple filtering (exchange/symbol only)
#[derive(Debug, Deserialize)]
pub struct SimpleQuery {
    /// Exchange name filter
    pub exchange: Option<String>,

    /// Symbol filter
    pub symbol: Option<String>,

    /// Limit for results
    #[serde(default = "default_limit")]
    pub limit: i64,
}

impl SimpleQuery {
    /// Validate required parameters
    pub fn validate(&self) -> Result<(), AppError> {
        // Validate exchange if provided
        if let Some(exchange) = &self.exchange {
            if exchange.trim().is_empty() {
                return Err(AppError::bad_request("Exchange parameter cannot be empty".to_string()));
            }
            if !exchange.chars().all(|c| c.is_alphanumeric() || c == '_') || exchange.len() > 50 {
                return Err(AppError::bad_request("Exchange must be alphanumeric (with underscores) and max 50 characters".to_string()));
            }
        }

        // Validate symbol if provided
        if let Some(symbol) = &self.symbol {
            if symbol.trim().is_empty() {
                return Err(AppError::bad_request("Symbol parameter cannot be empty".to_string()));
            }
            if !symbol.chars().all(|c| c.is_alphanumeric()) || symbol.len() > 20 {
                return Err(AppError::bad_request("Symbol must be alphanumeric and max 20 characters".to_string()));
            }
        }

        // Validate limit
        if self.limit < 1 || self.limit > 1000 {
            return Err(AppError::bad_request("Limit must be between 1 and 1000".to_string()));
        }

        Ok(())
    }

    /// Validate and cap the limit to maximum allowed
    pub fn validated_limit(&self) -> i64 {
        self.limit.min(500).max(1)
    }
}
