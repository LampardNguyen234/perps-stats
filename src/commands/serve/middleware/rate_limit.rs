use axum::{
    extract::ConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    body::Body,
};
use serde::Serialize;
use std::{net::SocketAddr, sync::Arc};

use crate::commands::serve::rate_limiter::RateLimiterBackend;

/// Extract client IP from request headers or connection info
fn extract_client_ip(headers: &axum::http::HeaderMap, addr: Option<SocketAddr>) -> String {
    // Try X-Forwarded-For header first (proxy support)
    if let Some(forwarded) = headers.get("x-forwarded-for") {
        if let Ok(header_val) = forwarded.to_str() {
            // Take the first IP from comma-separated list
            if let Some(first_ip) = header_val.split(',').next() {
                return first_ip.trim().to_string();
            }
        }
    }

    // Try X-Real-IP header
    if let Some(real_ip) = headers.get("x-real-ip") {
        if let Ok(header_val) = real_ip.to_str() {
            return header_val.to_string();
        }
    }

    // Fallback to socket address
    addr.map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Rate limit error response
#[derive(Debug, Serialize)]
struct RateLimitErrorResponse {
    error: String,
    message: String,
    limit: u32,
    window: String,
    retry_after_secs: u64,
}

impl IntoResponse for RateLimitErrorResponse {
    fn into_response(self) -> Response {
        let retry_after = self.retry_after_secs.to_string();
        let body = serde_json::to_string(&self).unwrap_or_default();

        (
            StatusCode::TOO_MANY_REQUESTS,
            [
                ("Content-Type", "application/json"),
                ("Retry-After", &retry_after),
            ],
            body,
        )
            .into_response()
    }
}

/// Rate limit check middleware with state
pub async fn rate_limit_check_state(
    state: axum::extract::State<Arc<dyn RateLimiterBackend>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    rate_limit_check(ConnectInfo(addr), req, next, state.0.clone()).await
}

/// Rate limit check middleware
async fn rate_limit_check(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
    backend: Arc<dyn RateLimiterBackend>,
) -> Response {
    let client_ip_str = extract_client_ip(req.headers(), Some(addr));

    // Parse IP address
    let client_ip = match client_ip_str.parse::<std::net::IpAddr>() {
        Ok(ip) => ip,
        Err(_) => {
            // If IP parsing fails, skip rate limiting for this request
            return next.run(req).await;
        }
    };

    // Get current time in milliseconds
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Check per-IP rate limit
    let per_ip_decision = match backend.check_rate_limit(client_ip, now_ms).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Rate limiter error: {}", e);
            return next.run(req).await;
        }
    };

    // Check global rate limit
    let global_decision = match backend.check_global_rate_limit(now_ms).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Global rate limiter error: {}", e);
            return next.run(req).await;
        }
    };

    // Use the most restrictive decision
    let decision = if !per_ip_decision.allowed {
        per_ip_decision
    } else {
        global_decision
    };

    // If rate limited, return 429 error
    if !decision.allowed {
        if let Some(window_state) = decision.primary_window() {
            let retry_after_ms = window_state
                .reset_at
                .saturating_sub(now_ms)
                .max(1);

            // Convert from milliseconds to seconds (ceiling division)
            let retry_after_secs = (retry_after_ms + 999) / 1000;

            let window_desc = format_window_description(window_state.window_secs);

            let error = RateLimitErrorResponse {
                error: "Rate limit exceeded".to_string(),
                message: format!(
                    "Too many requests. Please retry after {} seconds.",
                    retry_after_secs
                ),
                limit: window_state.limit,
                window: window_desc,
                retry_after_secs,
            };

            tracing::warn!(
                "Rate limit exceeded for IP {} (window: {}s, limit: {})",
                client_ip,
                window_state.window_secs,
                window_state.limit
            );

            return error.into_response();
        }
    }

    // Continue to next middleware/handler
    let mut response = next.run(req).await;

    // Add rate limit headers to response
    if let Some(window_state) = decision.primary_window() {
        let headers = response.headers_mut();
        if let Ok(limit) = window_state.limit.to_string().parse() {
            headers.insert("X-RateLimit-Limit", limit);
        }
        if let Ok(remaining) = window_state.remaining.to_string().parse() {
            headers.insert("X-RateLimit-Remaining", remaining);
        }
        if let Ok(reset) = window_state.reset_at.to_string().parse() {
            headers.insert("X-RateLimit-Reset", reset);
        }
    }

    response
}

/// Format window duration for human readability
fn format_window_description(window_secs: u64) -> String {
    match window_secs {
        1 => "per second".to_string(),
        2 => "per 2 seconds".to_string(),
        5 => "per 5 seconds".to_string(),
        10 => "per 10 seconds".to_string(),
        60 => "per minute".to_string(),
        300 => "per 5 minutes".to_string(),
        3600 => "per hour".to_string(),
        _ => format!("per {} seconds", window_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_window_description() {
        assert_eq!(format_window_description(1), "per second");
        assert_eq!(format_window_description(60), "per minute");
        assert_eq!(format_window_description(3600), "per hour");
        assert_eq!(format_window_description(42), "per 42 seconds");
    }

    #[test]
    fn test_extract_client_ip_from_x_forwarded_for() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "192.168.1.100, 10.0.0.1".parse().unwrap());

        let ip = extract_client_ip(&headers, None);
        assert_eq!(ip, "192.168.1.100");
    }

    #[test]
    fn test_extract_client_ip_from_x_real_ip() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-real-ip", "10.0.0.2".parse().unwrap());

        let ip = extract_client_ip(&headers, None);
        assert_eq!(ip, "10.0.0.2");
    }

    #[test]
    fn test_extract_client_ip_from_socket() {
        let headers = axum::http::HeaderMap::new();
        let addr = "127.0.0.1:8080".parse().ok();

        let ip = extract_client_ip(&headers, addr);
        assert_eq!(ip, "127.0.0.1");
    }
}
