//! Retry logic with exponential backoff for handling rate limit errors.
//!
//! This module provides a generic retry mechanism that can be used by all exchange clients
//! to automatically retry failed requests with exponential backoff.

use anyhow::Result;
use std::future::Future;

/// Configuration for retry behavior
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (does not include the initial attempt)
    pub max_retries: u32,
    /// Base delay in milliseconds for the first retry
    pub base_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 8,
            base_delay_ms: 1000, // 1 second
        }
    }
}

impl RetryConfig {
    /// Create a new retry configuration
    pub fn new(max_retries: u32, base_delay_ms: u64) -> Self {
        Self {
            max_retries,
            base_delay_ms,
        }
    }

    /// Create a retry configuration from environment variables
    /// Falls back to defaults if not set
    pub fn from_env() -> Self {
        let max_retries = std::env::var("RETRY_MAX_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);

        let base_delay_ms = std::env::var("RETRY_BASE_DELAY_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);

        Self {
            max_retries,
            base_delay_ms,
        }
    }
}

/// Execute a request with automatic retry on 429 (Too Many Requests) errors
///
/// This function will retry the request up to `config.max_retries` times with
/// exponential backoff. Only 429 errors trigger retries; all other errors are
/// returned immediately.
///
/// # Arguments
/// * `config` - Retry configuration (max retries and base delay)
/// * `request_fn` - Async function that performs the request
///
/// # Returns
/// The result of the request, or an error if all retries are exhausted
///
/// # Example
/// ```rust,no_run
/// use perps_core::retry::{execute_with_retry, RetryConfig};
///
/// async fn make_request() -> anyhow::Result<String> {
///     // Your HTTP request here
///     Ok("response".to_string())
/// }
///
/// # async fn example() -> anyhow::Result<()> {
/// let config = RetryConfig::default();
/// let result = execute_with_retry(&config, || make_request()).await?;
/// # Ok(())
/// # }
/// ```
pub async fn execute_with_retry<F, Fut, T>(
    config: &RetryConfig,
    mut request_fn: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    for attempt in 0..=config.max_retries {
        let result = request_fn().await;

        match result {
            Ok(data) => return Ok(data),
            Err(e) => {
                // Check if this is a 429 error
                let error_string = e.to_string();
                let is_429 = error_string.contains("429") || error_string.contains("Too Many Requests");

                if is_429 && attempt < config.max_retries {
                    // Calculate exponential backoff: base_delay * 2^attempt
                    let delay_ms = config.base_delay_ms * (1 << attempt);
                    tracing::warn!(
                        "Request failed with 429 error (attempt {}/{}), retrying in {}ms: {}",
                        attempt + 1,
                        config.max_retries + 1,
                        delay_ms,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                } else {
                    // Either not a 429 error, or we've exhausted retries
                    return Err(e);
                }
            }
        }
    }

    Err(anyhow::anyhow!("Unexpected: retry loop exhausted without returning"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_retry_success_on_first_attempt() {
        let config = RetryConfig::default();
        let result = execute_with_retry(&config, || async { Ok::<_, anyhow::Error>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_retry_success_after_failures() {
        let config = RetryConfig::new(3, 10); // Short delay for testing
        let attempt_count = Arc::new(AtomicU32::new(0));
        let attempt_count_clone = attempt_count.clone();

        let result = execute_with_retry(&config, || {
            let count = attempt_count_clone.clone();
            async move {
                let current = count.fetch_add(1, Ordering::SeqCst);
                if current < 2 {
                    Err(anyhow::anyhow!("429 Too Many Requests"))
                } else {
                    Ok::<_, anyhow::Error>(42)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3); // 2 failures + 1 success
    }

    #[tokio::test]
    async fn test_retry_exhausted() {
        let config = RetryConfig::new(2, 10); // Short delay for testing
        let attempt_count = Arc::new(AtomicU32::new(0));
        let attempt_count_clone = attempt_count.clone();

        let result = execute_with_retry(&config, || {
            let count = attempt_count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(anyhow::anyhow!("429 Too Many Requests"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3); // Initial + 2 retries
    }

    #[tokio::test]
    async fn test_non_429_error_no_retry() {
        let config = RetryConfig::default();
        let attempt_count = Arc::new(AtomicU32::new(0));
        let attempt_count_clone = attempt_count.clone();

        let result = execute_with_retry(&config, || {
            let count = attempt_count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(anyhow::anyhow!("500 Internal Server Error"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempt_count.load(Ordering::SeqCst), 1); // No retries for non-429
    }

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 8);
        assert_eq!(config.base_delay_ms, 1000);
    }

    #[test]
    fn test_retry_config_custom() {
        let config = RetryConfig::new(5, 2000);
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.base_delay_ms, 2000);
    }
}
