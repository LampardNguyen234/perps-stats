use std::collections::BTreeMap;
use std::time::Duration;

use super::config::RateLimitConfig;

/// Parse rate limit configuration from JSON string
///
/// Expects JSON format:
/// ```json
/// {
///   "per_ip": {"1": 50, "60": 2000},
///   "global": {"1": 100, "60": 20000}
/// }
/// ```
pub fn parse_json_config(json_str: &str) -> anyhow::Result<RateLimitConfig> {
    let value: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Invalid rate limit JSON: {}", e))?;

    let mut per_ip_limits = BTreeMap::new();
    let mut global_limits = BTreeMap::new();

    // Parse per_ip limits
    if let Some(per_ip) = value.get("per_ip").and_then(|v| v.as_object()) {
        for (window_str, limit_val) in per_ip {
            let window_secs: u64 = window_str.parse()
                .map_err(|_| anyhow::anyhow!("Invalid window duration in per_ip: {}", window_str))?;

            let limit: u32 = limit_val.as_u64()
                .ok_or_else(|| anyhow::anyhow!("Invalid limit value in per_ip: {}", limit_val))?
                as u32;

            per_ip_limits.insert(Duration::from_secs(window_secs), limit);
        }
    }

    // Parse global limits
    if let Some(global) = value.get("global").and_then(|v| v.as_object()) {
        for (window_str, limit_val) in global {
            let window_secs: u64 = window_str.parse()
                .map_err(|_| anyhow::anyhow!("Invalid window duration in global: {}", window_str))?;

            let limit: u32 = limit_val.as_u64()
                .ok_or_else(|| anyhow::anyhow!("Invalid limit value in global: {}", limit_val))?
                as u32;

            global_limits.insert(Duration::from_secs(window_secs), limit);
        }
    }

    // Get max_tracked_ips if specified
    let max_tracked_ips = value.get("max_tracked_ips")
        .and_then(|v| v.as_u64())
        .unwrap_or(10000) as usize;

    let config = RateLimitConfig {
        per_ip_limits,
        global_limits,
        max_tracked_ips,
    };

    config.validate()
        .map_err(|e| anyhow::anyhow!("Invalid rate limit configuration: {}", e))?;

    Ok(config)
}

/// Build rate limit configuration from individual CLI flags
pub fn build_from_flags(
    json_config: Option<String>,
    per_ip_second: u32,
    per_ip_2sec: u32,
    per_ip_minute: u32,
    global_second: u32,
    global_minute: u32,
    max_ips: usize,
) -> anyhow::Result<RateLimitConfig> {
    // If JSON config provided, use that (takes precedence)
    if let Some(json_str) = json_config {
        return parse_json_config(&json_str);
    }

    // Otherwise, build from individual flags
    let mut per_ip_limits = BTreeMap::new();
    let mut global_limits = BTreeMap::new();

    // Per-IP limits
    if per_ip_second > 0 {
        per_ip_limits.insert(Duration::from_secs(1), per_ip_second);
    }
    if per_ip_2sec > 0 {
        per_ip_limits.insert(Duration::from_secs(2), per_ip_2sec);
    }
    if per_ip_minute > 0 {
        per_ip_limits.insert(Duration::from_secs(60), per_ip_minute);
    }

    // Global limits
    if global_second > 0 {
        global_limits.insert(Duration::from_secs(1), global_second);
    }
    if global_minute > 0 {
        global_limits.insert(Duration::from_secs(60), global_minute);
    }

    let config = RateLimitConfig {
        per_ip_limits,
        global_limits,
        max_tracked_ips: max_ips,
    };

    config.validate()
        .map_err(|e| anyhow::anyhow!("Invalid rate limit configuration: {}", e))?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_config() {
        let json = r#"{"per_ip":{"1":50,"60":2000},"global":{"1":100,"60":20000}}"#;
        let config = parse_json_config(json).unwrap();

        assert_eq!(config.per_ip_limits.len(), 2);
        assert_eq!(config.global_limits.len(), 2);
        assert_eq!(
            config.per_ip_limits.get(&Duration::from_secs(1)),
            Some(&50)
        );
        assert_eq!(
            config.per_ip_limits.get(&Duration::from_secs(60)),
            Some(&2000)
        );
    }

    #[test]
    fn test_parse_json_config_with_custom_windows() {
        let json = r#"{"per_ip":{"1":10,"2":80,"5":150,"60":2000},"global":{"1":100,"60":20000}}"#;
        let config = parse_json_config(json).unwrap();

        assert_eq!(config.per_ip_limits.len(), 4);
        assert_eq!(config.per_ip_limits.get(&Duration::from_secs(2)), Some(&80));
        assert_eq!(config.per_ip_limits.get(&Duration::from_secs(5)), Some(&150));
    }

    #[test]
    fn test_build_from_flags() {
        let config = build_from_flags(
            None,
            50,  // per_ip_second
            0,   // per_ip_2sec (disabled)
            2000, // per_ip_minute
            100, // global_second
            20000, // global_minute
            10000, // max_ips
        ).unwrap();

        assert_eq!(config.per_ip_limits.len(), 2);
        assert_eq!(config.global_limits.len(), 2);
        assert_eq!(config.max_tracked_ips, 10000);
    }

    #[test]
    fn test_build_from_flags_with_2sec() {
        let config = build_from_flags(
            None,
            50,  // per_ip_second
            80,  // per_ip_2sec (enabled)
            2000, // per_ip_minute
            100, // global_second
            20000, // global_minute
            10000, // max_ips
        ).unwrap();

        assert_eq!(config.per_ip_limits.len(), 3); // 1s, 2s, 60s
        assert_eq!(config.per_ip_limits.get(&Duration::from_secs(2)), Some(&80));
    }

    #[test]
    fn test_json_config_takes_precedence() {
        let json = r#"{"per_ip":{"1":10},"global":{"1":20}}"#;
        let config = build_from_flags(
            Some(json.to_string()),
            50,  // these should be ignored
            0,
            2000,
            100,
            20000,
            10000,
        ).unwrap();

        // Should use JSON config, not flags
        assert_eq!(config.per_ip_limits.get(&Duration::from_secs(1)), Some(&10));
        assert_eq!(config.global_limits.get(&Duration::from_secs(1)), Some(&20));
        assert_eq!(config.per_ip_limits.len(), 1);
    }

    #[test]
    fn test_invalid_json() {
        let result = parse_json_config("not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_window_duration() {
        let json = r#"{"per_ip":{"invalid":50}}"#;
        let result = parse_json_config(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_config_invalid() {
        let json = r#"{"per_ip":{},"global":{}}"#;
        let result = parse_json_config(json);
        assert!(result.is_err());
    }
}
