//! Token bucket rate limiter with sliding window tracking
//!
//! This module provides a robust rate limiting solution that tracks the number of requests
//! within sliding time windows and supports multiple concurrent rate limits.
//!
//! Instead of adding fixed delays between requests, this rate limiter allows bursts of requests
//! up to the configured limit, then enforces waiting periods when limits are reached.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// A single rate limit constraint (e.g., "30 requests per second")
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    /// Maximum number of requests allowed
    pub max_requests: usize,
    /// Time window for the limit
    pub window: Duration,
}

impl RateLimit {
    /// Create a new rate limit
    ///
    /// # Arguments
    /// * `max_requests` - Maximum number of requests allowed
    /// * `window` - Time window for the limit
    ///
    /// # Example
    /// ```rust
    /// use perps_core::RateLimit;
    /// use std::time::Duration;
    ///
    /// // 30 requests per second
    /// let limit = RateLimit::new(30, Duration::from_secs(1));
    /// ```
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            max_requests,
            window,
        }
    }

    /// Create a rate limit expressed in requests per second
    ///
    /// # Example
    /// ```rust
    /// use perps_core::RateLimit;
    ///
    /// let limit = RateLimit::per_second(30); // 30 requests per second
    /// ```
    pub fn per_second(max_requests: usize) -> Self {
        Self::new(max_requests, Duration::from_secs(1))
    }

    /// Create a rate limit expressed in requests per minute
    ///
    /// # Example
    /// ```rust
    /// use perps_core::RateLimit;
    ///
    /// let limit = RateLimit::per_minute(1200); // 1200 requests per minute
    /// ```
    pub fn per_minute(max_requests: usize) -> Self {
        Self::new(max_requests, Duration::from_secs(60))
    }
}

/// Token bucket rate limiter with sliding window tracking
///
/// This rate limiter tracks timestamps of recent requests in a sliding window
/// and supports multiple concurrent rate limits. It allows bursts of requests
/// up to the configured limit, then enforces waiting when limits are reached.
///
/// # Example
/// ```rust
/// use perps_core::{RateLimiter, RateLimit};
///
/// #[tokio::main]
/// async fn main() {
///     // 30 requests per second, 60 requests per minute
///     let rate_limiter = RateLimiter::new(vec![
///         RateLimit::per_second(30),
///         RateLimit::per_minute(60),
///     ]);
///
///     // Execute API calls - first 30 execute immediately, then rate limited
///     for i in 0..100 {
///         rate_limiter.execute(|| async move {
///             println!("Request {}", i);
///             Ok::<_, anyhow::Error>(())
///         }).await.unwrap();
///     }
/// }
/// ```
#[derive(Clone)]
pub struct RateLimiter {
    /// List of rate limits to enforce (e.g., per second, per minute)
    limits: Vec<RateLimit>,
    /// Timestamps of recent requests (one queue per rate limit)
    request_history: Arc<Mutex<Vec<VecDeque<Instant>>>>,
}

impl RateLimiter {
    /// Create a new rate limiter with the specified rate limits
    ///
    /// # Arguments
    /// * `limits` - Vector of rate limits to enforce
    ///
    /// # Example
    /// ```rust
    /// use perps_core::{RateLimiter, RateLimit};
    ///
    /// // 30 requests per second AND 60 requests per minute
    /// let rate_limiter = RateLimiter::new(vec![
    ///     RateLimit::per_second(30),
    ///     RateLimit::per_minute(60),
    /// ]);
    /// ```
    pub fn new(limits: Vec<RateLimit>) -> Self {
        let history = vec![VecDeque::new(); limits.len()];
        Self {
            limits,
            request_history: Arc::new(Mutex::new(history)),
        }
    }

    /// Create preset rate limiter for Binance
    /// - 2400 requests per minute
    /// - 1200 requests per 5 minutes (safety margin)
    pub fn binance() -> Self {
        Self::new(vec![RateLimit::per_minute(2000)])
    }

    /// Create preset rate limiter for Bybit
    /// - 120 requests per second
    /// - 7200 requests per minute (safety margin)
    pub fn bybit() -> Self {
        Self::new(vec![
            RateLimit::per_second(120),
            RateLimit::per_minute(7200),
        ])
    }

    /// Create preset rate limiter for Hyperliquid
    /// - 1200 requests per minute (20 per second)
    /// - 6000 requests per 5 minutes (safety margin)
    pub fn hyperliquid() -> Self {
        Self::new(vec![RateLimit::per_second(10), RateLimit::per_minute(300)])
    }

    /// Create preset rate limiter for KuCoin
    /// - 30 requests per second
    /// - 1800 requests per minute (safety margin)
    pub fn kucoin() -> Self {
        Self::new(vec![RateLimit::per_second(10), RateLimit::per_minute(1000)])
    }

    /// Create preset rate limiter for Lighter
    /// - No strict limits documented, using conservative values
    /// - 10 requests per second
    /// - 600 requests per minute
    pub fn lighter() -> Self {
        Self::new(vec![RateLimit::per_second(10), RateLimit::per_minute(50)])
    }

    /// Create preset rate limiter for Paradex
    /// - 10 requests per second per IP
    /// - 600 requests per minute (safety margin)
    pub fn paradex() -> Self {
        Self::new(vec![RateLimit::per_second(10), RateLimit::per_minute(1000)])
    }

    /// Create preset rate limiter for Aster DEX
    /// - 2400 request weight per minute (40 per second)
    /// - 1200 orders per minute (safety margin)
    pub fn aster() -> Self {
        Self::new(vec![RateLimit::per_second(40), RateLimit::per_minute(2400)])
    }

    /// Create preset rate limiter for Extended Exchange
    /// - 1000 requests per minute per IP (~16.67 per second)
    /// - Using conservative 16 requests per second to stay under limit
    pub fn extended() -> Self {
        Self::new(vec![RateLimit::per_second(16), RateLimit::per_minute(1000)])
    }

    /// Create preset rate limiter for Pacifica
    /// - Conservative limits until actual limits are documented
    /// - 20 requests per second
    /// - 1200 requests per minute
    pub fn pacifica() -> Self {
        Self::new(vec![RateLimit::per_second(20), RateLimit::per_minute(1200)])
    }

    /// Create preset rate limiter for Nano
    /// Conservative rate limit based on Nano API documentation:
    /// - /orderbook: 2400/min (40 req/sec)
    /// - /assets: 1200/min (20 req/sec)
    /// - Using conservative 20 requests per second for all endpoints
    pub fn nado() -> Self {
        Self::new(vec![RateLimit::per_second(20), RateLimit::per_minute(1200)])
    }

    /// Create a disabled rate limiter (no limits)
    /// Useful for testing or when rate limiting is not needed
    pub fn disabled() -> Self {
        Self::new(vec![])
    }

    /// Remove expired timestamps from history (outside the sliding window)
    fn clean_history(&self, history: &mut VecDeque<Instant>, window: Duration, now: Instant) {
        while let Some(timestamp) = history.front() {
            if now.duration_since(*timestamp) >= window {
                history.pop_front();
            } else {
                break;
            }
        }
    }

    /// Calculate how long to wait before the next request can be made
    ///
    /// Returns Duration::ZERO if the request can be made immediately,
    /// otherwise returns the minimum time to wait to satisfy all rate limits.
    async fn calculate_wait_time(&self) -> Duration {
        if self.limits.is_empty() {
            return Duration::ZERO;
        }

        let mut history = self.request_history.lock().await;
        let now = Instant::now();
        let mut max_wait = Duration::ZERO;

        for (i, limit) in self.limits.iter().enumerate() {
            // Clean up old timestamps outside the window
            self.clean_history(&mut history[i], limit.window, now);

            // Check if we've reached the limit
            if history[i].len() >= limit.max_requests {
                // We've hit the limit - need to wait until the oldest request expires
                if let Some(oldest) = history[i].front() {
                    let elapsed = now.duration_since(*oldest);
                    if elapsed < limit.window {
                        let wait = limit.window - elapsed;
                        max_wait = max_wait.max(wait);
                    }
                }
            }
        }

        max_wait
    }

    /// Execute a request with automatic rate limiting
    ///
    /// This method will automatically add delays to respect all configured rate limits.
    /// If any limit is reached, it will sleep for the required duration before executing.
    ///
    /// Note: Retry logic for 429 errors should be implemented at a higher level
    /// (e.g., in exchange client HTTP methods) since FnOnce can only be called once.
    ///
    /// # Arguments
    /// * `request_fn` - Async function that performs the API request
    ///
    /// # Returns
    /// The result of the request function
    ///
    /// # Example
    /// ```rust
    /// use perps_core::{RateLimiter, RateLimit};
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let rate_limiter = RateLimiter::new(vec![RateLimit::per_second(5)]);
    ///
    ///     let result = rate_limiter.execute(|| async {
    ///         // Your API call here
    ///         reqwest::get("https://api.example.com/data").await
    ///     }).await;
    /// }
    /// ```
    pub async fn execute<F, Fut, T>(&self, request_fn: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        // Calculate wait time
        let wait_time = self.calculate_wait_time().await;

        if wait_time > Duration::ZERO {
            tracing::debug!(
                "Rate limiter: waiting {}ms before next request",
                wait_time.as_millis()
            );
            tokio::time::sleep(wait_time).await;
        }

        // Record this request timestamp in all histories
        {
            let mut history = self.request_history.lock().await;
            let now = Instant::now();
            for queue in history.iter_mut() {
                queue.push_back(now);
            }
        }

        // Execute request
        request_fn().await
    }

    /// Get the configured rate limits
    pub fn limits(&self) -> &[RateLimit] {
        &self.limits
    }

    /// Get statistics about the rate limiter
    pub async fn stats(&self) -> RateLimiterStats {
        let history = self.request_history.lock().await;
        let now = Instant::now();

        let mut limit_stats = Vec::new();
        for (i, limit) in self.limits.iter().enumerate() {
            // Count requests within the window
            let requests_in_window = history[i]
                .iter()
                .filter(|&&timestamp| now.duration_since(timestamp) < limit.window)
                .count();

            limit_stats.push(LimitStats {
                limit: *limit,
                requests_in_window,
                remaining: limit.max_requests.saturating_sub(requests_in_window),
            });
        }

        RateLimiterStats {
            limits: limit_stats,
            total_requests: history.first().map(|h| h.len()).unwrap_or(0),
        }
    }
}

/// Statistics for a single rate limit
#[derive(Debug, Clone)]
pub struct LimitStats {
    /// The rate limit configuration
    pub limit: RateLimit,
    /// Number of requests currently in the sliding window
    pub requests_in_window: usize,
    /// Number of remaining requests before hitting the limit
    pub remaining: usize,
}

/// Statistics for the rate limiter
#[derive(Debug, Clone)]
pub struct RateLimiterStats {
    /// Statistics for each configured rate limit
    pub limits: Vec<LimitStats>,
    /// Total number of requests made (all time)
    pub total_requests: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn test_rate_limiter_burst() {
        // 5 requests per second
        let rate_limiter = RateLimiter::new(vec![RateLimit::per_second(5)]);

        let start = Instant::now();

        // First 5 requests should execute immediately (burst)
        for i in 0..5 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
            println!("Request {} at {}ms", i, start.elapsed().as_millis());
        }

        let after_burst = Instant::now();
        let burst_duration = after_burst.duration_since(start);

        // All 5 requests should complete very quickly (burst)
        assert!(
            burst_duration < Duration::from_millis(100),
            "Burst took {}ms, expected < 100ms",
            burst_duration.as_millis()
        );

        // 6th request should wait ~1 second
        rate_limiter
            .execute(|| async { Ok::<_, anyhow::Error>(()) })
            .await
            .unwrap();

        let after_sixth = Instant::now();
        let total_duration = after_sixth.duration_since(start);

        // Should have waited ~1 second for the 6th request
        assert!(
            total_duration >= Duration::from_millis(950),
            "Total duration {}ms, expected >= 950ms",
            total_duration.as_millis()
        );
        assert!(
            total_duration < Duration::from_millis(1150),
            "Total duration {}ms, expected < 1150ms",
            total_duration.as_millis()
        );
    }

    #[tokio::test]
    async fn test_rate_limiter_multiple_limits() {
        // 5 requests per second AND 8 requests per 2 seconds
        let rate_limiter = RateLimiter::new(vec![
            RateLimit::per_second(5),
            RateLimit::new(8, Duration::from_secs(2)),
        ]);

        let start = Instant::now();

        // First 5 requests should execute immediately (burst)
        for _ in 0..5 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
        }

        let after_first_burst = start.elapsed();
        assert!(after_first_burst < Duration::from_millis(100));

        // Next 3 requests (total 8 in 2 seconds) should wait
        for _ in 0..3 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
        }

        let after_eight = start.elapsed();
        // Should have waited at least 1 second (for the per-second limit to reset)
        assert!(after_eight >= Duration::from_millis(950));

        // 9th request should wait for 2-second window to expire
        rate_limiter
            .execute(|| async { Ok::<_, anyhow::Error>(()) })
            .await
            .unwrap();

        let after_ninth = start.elapsed();
        // Should have waited at least 2 seconds total
        assert!(
            after_ninth >= Duration::from_millis(1950),
            "Total duration {}ms, expected >= 1950ms",
            after_ninth.as_millis()
        );
    }

    #[tokio::test]
    async fn test_rate_limiter_disabled() {
        let rate_limiter = RateLimiter::disabled();

        let start = Instant::now();

        // Execute 100 requests - should all be instant
        for _ in 0..100 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
        }

        let elapsed = start.elapsed();

        // Should complete very quickly (allow 100ms tolerance)
        assert!(elapsed < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn test_rate_limiter_stats() {
        let rate_limiter = RateLimiter::new(vec![RateLimit::per_second(5)]);

        // Before any requests
        let stats = rate_limiter.stats().await;
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.limits.len(), 1);
        assert_eq!(stats.limits[0].requests_in_window, 0);
        assert_eq!(stats.limits[0].remaining, 5);

        // After 3 requests
        for _ in 0..3 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
        }

        let stats = rate_limiter.stats().await;
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.limits[0].requests_in_window, 3);
        assert_eq!(stats.limits[0].remaining, 2);

        // After hitting the limit (5 requests)
        for _ in 0..2 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
        }

        let stats = rate_limiter.stats().await;
        assert_eq!(stats.total_requests, 5);
        assert_eq!(stats.limits[0].requests_in_window, 5);
        assert_eq!(stats.limits[0].remaining, 0);
    }

    #[test]
    fn test_preset_rate_limiters() {
        let binance = RateLimiter::binance();
        assert_eq!(binance.limits().len(), 1);
        assert_eq!(binance.limits()[0].max_requests, 2000);
        assert_eq!(binance.limits()[0].window, Duration::from_secs(60));

        let bybit = RateLimiter::bybit();
        assert_eq!(bybit.limits().len(), 2);
        assert_eq!(bybit.limits()[0].max_requests, 120);
        assert_eq!(bybit.limits()[0].window, Duration::from_secs(1));

        let hyperliquid = RateLimiter::hyperliquid();
        assert_eq!(hyperliquid.limits().len(), 2);
        assert_eq!(hyperliquid.limits()[0].max_requests, 10);
        assert_eq!(hyperliquid.limits()[0].window, Duration::from_secs(1));

        let kucoin = RateLimiter::kucoin();
        assert_eq!(kucoin.limits().len(), 2);
        assert_eq!(kucoin.limits()[0].max_requests, 10);
        assert_eq!(kucoin.limits()[0].window, Duration::from_secs(1));

        let lighter = RateLimiter::lighter();
        assert_eq!(lighter.limits().len(), 2);
        assert_eq!(lighter.limits()[0].max_requests, 10);

        let paradex = RateLimiter::paradex();
        assert_eq!(paradex.limits().len(), 2);
        assert_eq!(paradex.limits()[0].max_requests, 10);

        let disabled = RateLimiter::disabled();
        assert_eq!(disabled.limits().len(), 0);
    }

    #[tokio::test]
    async fn test_sliding_window_cleanup() {
        // 5 requests per 500ms (very short window for testing)
        let rate_limiter = RateLimiter::new(vec![RateLimit::new(5, Duration::from_millis(500))]);

        // Make 5 requests (should be instant)
        for _ in 0..5 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
        }

        // Wait for window to expire
        tokio::time::sleep(Duration::from_millis(600)).await;

        // Should be able to make 5 more requests instantly
        let start = Instant::now();
        for _ in 0..5 {
            rate_limiter
                .execute(|| async { Ok::<_, anyhow::Error>(()) })
                .await
                .unwrap();
        }
        let elapsed = start.elapsed();

        // All 5 should complete quickly after window reset
        assert!(
            elapsed < Duration::from_millis(100),
            "Expected < 100ms, got {}ms",
            elapsed.as_millis()
        );
    }
}
