use super::backend::{RateLimitDecision, RateLimitError, RateLimiterBackend, WindowState};
use super::config::RateLimitConfig;
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

pub struct InMemoryBackend {
    config: Arc<RateLimitConfig>,
    ip_state: DashMap<IpAddr, IpRateState>,
    global_state: Arc<parking_lot::RwLock<GlobalRateState>>,
}

struct IpRateState {
    windows: BTreeMap<u64, SlidingWindow>,
    last_access_ms: u64,
}

struct GlobalRateState {
    windows: BTreeMap<u64, SlidingWindow>,
}

struct SlidingWindow {
    timestamps: Vec<u64>,
    limit: u32,
    window_ms: u64,
}

impl SlidingWindow {
    fn new(limit: u32, window: Duration) -> Self {
        Self {
            timestamps: Vec::new(),
            limit,
            window_ms: window.as_millis() as u64,
        }
    }

    fn cleanup(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(self.window_ms);
        self.timestamps.retain(|&ts| ts >= cutoff);
    }

    fn check_and_increment(&mut self, now_ms: u64) -> (bool, u32) {
        self.cleanup(now_ms);
        let count = self.timestamps.len() as u32;
        if count < self.limit {
            self.timestamps.push(now_ms);
            (true, self.limit - count - 1)
        } else {
            (false, 0)
        }
    }

    fn reset_at(&self, now_ms: u64) -> u64 {
        self.timestamps
            .first()
            .map(|&oldest| oldest + self.window_ms)
            .unwrap_or(now_ms + self.window_ms)
    }
}

impl InMemoryBackend {
    pub fn new(config: RateLimitConfig) -> Self {
        let mut global_windows = BTreeMap::new();
        for (duration, limit) in &config.global_limits {
            let window_secs = duration.as_secs();
            global_windows.insert(window_secs, SlidingWindow::new(*limit, *duration));
        }

        Self {
            config: Arc::new(config),
            ip_state: DashMap::new(),
            global_state: Arc::new(parking_lot::RwLock::new(GlobalRateState {
                windows: global_windows,
            })),
        }
    }

    fn get_or_create_ip_state(&self, ip: IpAddr, now_ms: u64) -> Arc<IpRateState> {
        let config = self.config.clone();
        Arc::new(
            self.ip_state
                .entry(ip)
                .or_insert_with(|| {
                    let mut windows = BTreeMap::new();
                    for (duration, limit) in &config.per_ip_limits {
                        let window_secs = duration.as_secs();
                        windows
                            .insert(window_secs, SlidingWindow::new(*limit, *duration));
                    }
                    IpRateState {
                        windows,
                        last_access_ms: now_ms,
                    }
                })
                .value()
                .clone(),
        )
    }

    fn evict_if_needed(&self) {
        if self.ip_state.len() > self.config.max_tracked_ips {
            if let Some(entry) = self.ip_state.iter().next() {
                self.ip_state.remove(entry.key());
            }
        }
    }
}

impl Clone for IpRateState {
    fn clone(&self) -> Self {
        Self {
            windows: self.windows.clone(),
            last_access_ms: self.last_access_ms,
        }
    }
}

impl Clone for SlidingWindow {
    fn clone(&self) -> Self {
        Self {
            timestamps: self.timestamps.clone(),
            limit: self.limit,
            window_ms: self.window_ms,
        }
    }
}

#[async_trait::async_trait]
impl RateLimiterBackend for InMemoryBackend {
    async fn check_rate_limit(
        &self,
        ip: IpAddr,
        now_ms: u64,
    ) -> Result<RateLimitDecision, RateLimitError> {
        let mut decision = RateLimitDecision {
            allowed: true,
            windows: BTreeMap::new(),
            most_restrictive_window: None,
        };

        // Check if entry exists and update it
        if let Some(mut entry) = self.ip_state.get_mut(&ip) {
            for (window_secs, window) in &mut entry.windows {
                let (allowed, remaining) = window.check_and_increment(now_ms);
                if !allowed {
                    decision.allowed = false;
                    decision.most_restrictive_window = Some(*window_secs);
                }

                decision.windows.insert(
                    *window_secs,
                    WindowState {
                        limit: window.limit,
                        remaining,
                        reset_at: window.reset_at(now_ms),
                        window_secs: *window_secs,
                    },
                );
            }
            entry.last_access_ms = now_ms;
        } else {
            // Entry doesn't exist, create it
            let mut windows = BTreeMap::new();
            for (duration, limit) in &self.config.per_ip_limits {
                let window_secs = duration.as_secs();
                windows.insert(window_secs, SlidingWindow::new(*limit, *duration));
            }

            let mut new_state = IpRateState {
                windows,
                last_access_ms: now_ms,
            };

            for (window_secs, window) in &mut new_state.windows {
                let (allowed, remaining) = window.check_and_increment(now_ms);
                if !allowed {
                    decision.allowed = false;
                    decision.most_restrictive_window = Some(*window_secs);
                }

                decision.windows.insert(
                    *window_secs,
                    WindowState {
                        limit: window.limit,
                        remaining,
                        reset_at: window.reset_at(now_ms),
                        window_secs: *window_secs,
                    },
                );
            }

            self.ip_state.insert(ip, new_state);
        }

        self.evict_if_needed();
        Ok(decision)
    }

    async fn check_global_rate_limit(
        &self,
        now_ms: u64,
    ) -> Result<RateLimitDecision, RateLimitError> {
        let mut global = self.global_state.write();

        let mut decision = RateLimitDecision {
            allowed: true,
            windows: BTreeMap::new(),
            most_restrictive_window: None,
        };

        for (window_secs, window) in &mut global.windows {
            let (allowed, remaining) = window.check_and_increment(now_ms);
            if !allowed {
                decision.allowed = false;
                decision.most_restrictive_window = Some(*window_secs);
            }

            decision.windows.insert(
                *window_secs,
                WindowState {
                    limit: window.limit,
                    remaining,
                    reset_at: window.reset_at(now_ms),
                    window_secs: *window_secs,
                },
            );
        }

        Ok(decision)
    }

    async fn reset(&self) -> Result<(), RateLimitError> {
        self.ip_state.clear();
        self.global_state.write().windows.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_single_ip_under_limit() {
        let config = RateLimitConfig::builder()
            .add_per_ip_limit(Duration::from_secs(1), 5)
            .build()
            .unwrap();

        let backend = InMemoryBackend::new(config);
        let ip = "127.0.0.1".parse().unwrap();

        for _ in 0..5 {
            let decision = backend.check_rate_limit(ip, 1000).await.unwrap();
            assert!(decision.allowed);
        }
    }

    #[tokio::test]
    async fn test_single_ip_over_limit() {
        let config = RateLimitConfig::builder()
            .add_per_ip_limit(Duration::from_secs(1), 3)
            .build()
            .unwrap();

        let backend = InMemoryBackend::new(config);
        let ip = "127.0.0.1".parse().unwrap();

        for _ in 0..3 {
            backend.check_rate_limit(ip, 1000).await.unwrap();
        }

        let decision = backend.check_rate_limit(ip, 1000).await.unwrap();
        assert!(!decision.allowed);
    }

    #[tokio::test]
    async fn test_global_limit() {
        let config = RateLimitConfig::builder()
            .add_global_limit(Duration::from_secs(1), 5)
            .build()
            .unwrap();

        let backend = InMemoryBackend::new(config);

        for _ in 0..5 {
            let decision = backend.check_global_rate_limit(1000).await.unwrap();
            assert!(decision.allowed);
        }

        let decision = backend.check_global_rate_limit(1000).await.unwrap();
        assert!(!decision.allowed);
    }

    #[tokio::test]
    async fn test_window_reset() {
        let config = RateLimitConfig::builder()
            .add_per_ip_limit(Duration::from_secs(1), 2)
            .build()
            .unwrap();

        let backend = InMemoryBackend::new(config);
        let ip = "127.0.0.1".parse().unwrap();

        backend.check_rate_limit(ip, 1000).await.unwrap();
        backend.check_rate_limit(ip, 1000).await.unwrap();

        let decision = backend.check_rate_limit(ip, 1000).await.unwrap();
        assert!(!decision.allowed);

        let decision = backend.check_rate_limit(ip, 2100).await.unwrap();
        assert!(decision.allowed);
    }
}
