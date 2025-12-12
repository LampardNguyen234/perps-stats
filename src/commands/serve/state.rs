use perps_database::PostgresRepository;
use sqlx::PgPool;
use std::sync::Arc;

use crate::commands::serve::rate_limiter::{
    memory::InMemoryBackend, RateLimiterBackend,
};

/// Shared application state passed to all handlers
#[derive(Clone)]
pub struct AppState {
    /// Database connection pool
    pub pool: PgPool,

    /// Repository for database operations
    pub repository: Arc<PostgresRepository>,

    /// Rate limiter backend
    pub rate_limiter: Arc<dyn RateLimiterBackend>,
}

impl AppState {
    /// Create new AppState with database connection and rate limiter
    pub async fn new(database_url: &str) -> anyhow::Result<Self> {
        tracing::info!("Connecting to database: {}", database_url);

        let pool = PgPool::connect(database_url).await?;
        let repository = Arc::new(PostgresRepository::new(pool.clone()));

        tracing::info!("Database connection established");

        // Initialize rate limiter with default configuration
        let rate_limiter = Arc::new(InMemoryBackend::new(
            crate::commands::serve::rate_limiter::RateLimitConfig::default(),
        )) as Arc<dyn RateLimiterBackend>;

        tracing::info!("Rate limiter initialized");

        Ok(Self {
            pool,
            repository,
            rate_limiter,
        })
    }
}
