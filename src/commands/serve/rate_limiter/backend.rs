use std::collections::BTreeMap;
use std::net::IpAddr;

#[async_trait::async_trait]
pub trait RateLimiterBackend: Send + Sync {
    async fn check_rate_limit(
        &self,
        ip: IpAddr,
        now_ms: u64,
    ) -> Result<RateLimitDecision, RateLimitError>;

    async fn check_global_rate_limit(
        &self,
        now_ms: u64,
    ) -> Result<RateLimitDecision, RateLimitError>;

    async fn reset(&self) -> Result<(), RateLimitError>;
}

#[derive(Debug, Clone)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub windows: BTreeMap<u64, WindowState>,
    pub most_restrictive_window: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WindowState {
    pub limit: u32,
    pub remaining: u32,
    pub reset_at: u64,
    pub window_secs: u64,
}

impl RateLimitDecision {
    pub fn primary_window(&self) -> Option<&WindowState> {
        self.most_restrictive_window
            .and_then(|window_secs| self.windows.get(&window_secs))
    }

    pub fn retry_after_secs(&self, now_ms: u64) -> u64 {
        self.primary_window()
            .map(|w| w.reset_at.saturating_sub(now_ms) / 1000 + 1)
            .unwrap_or(1)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RateLimitError {
    #[error("Backend error: {0}")]
    BackendError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),
}
