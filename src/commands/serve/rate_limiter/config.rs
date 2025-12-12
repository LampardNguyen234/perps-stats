use std::collections::BTreeMap;
use std::time::Duration;

/// Flexible rate limit configuration supporting arbitrary time windows
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub per_ip_limits: BTreeMap<Duration, u32>,
    pub global_limits: BTreeMap<Duration, u32>,
    pub max_tracked_ips: usize,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        let mut per_ip = BTreeMap::new();
        per_ip.insert(Duration::from_secs(1), 50);
        per_ip.insert(Duration::from_secs(60), 1000);

        let mut global = BTreeMap::new();
        global.insert(Duration::from_secs(1), 1000);
        global.insert(Duration::from_secs(60), 20000);

        Self {
            per_ip_limits: per_ip,
            global_limits: global,
            max_tracked_ips: 10000,
        }
    }
}

impl RateLimitConfig {
    pub fn builder() -> RateLimitConfigBuilder {
        RateLimitConfigBuilder::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.per_ip_limits.is_empty() && self.global_limits.is_empty() {
            return Err("At least one rate limit must be configured".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct RateLimitConfigBuilder {
    per_ip_limits: BTreeMap<Duration, u32>,
    global_limits: BTreeMap<Duration, u32>,
    max_tracked_ips: Option<usize>,
}

impl RateLimitConfigBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_per_ip_limit(mut self, window: Duration, limit: u32) -> Self {
        self.per_ip_limits.insert(window, limit);
        self
    }

    pub fn add_global_limit(mut self, window: Duration, limit: u32) -> Self {
        self.global_limits.insert(window, limit);
        self
    }

    pub fn max_tracked_ips(mut self, max: usize) -> Self {
        self.max_tracked_ips = Some(max);
        self
    }

    pub fn build(self) -> Result<RateLimitConfig, String> {
        let config = RateLimitConfig {
            per_ip_limits: self.per_ip_limits,
            global_limits: self.global_limits,
            max_tracked_ips: self.max_tracked_ips.unwrap_or(10000),
        };
        config.validate()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RateLimitConfig::default();
        assert_eq!(config.per_ip_limits.len(), 2);
        assert_eq!(config.global_limits.len(), 2);
        assert_eq!(config.max_tracked_ips, 10000);
    }

    #[test]
    fn test_builder() {
        let config = RateLimitConfig::builder()
            .add_per_ip_limit(Duration::from_secs(1), 50)
            .add_per_ip_limit(Duration::from_secs(60), 2000)
            .build()
            .unwrap();

        assert_eq!(config.per_ip_limits.len(), 2);
    }
}
