use perps_database::PostgresRepository;
use sqlx::PgPool;
use std::sync::Arc;

/// Shared application state passed to all handlers
#[derive(Clone)]
pub struct AppState {
    /// Database connection pool
    pub pool: PgPool,

    /// Repository for database operations
    pub repository: Arc<PostgresRepository>,
}

impl AppState {
    /// Create new AppState with database connection
    pub async fn new(database_url: &str) -> anyhow::Result<Self> {
        tracing::info!("Connecting to database: {}", database_url);

        let pool = PgPool::connect(database_url).await?;
        let repository = Arc::new(PostgresRepository::new(pool.clone()));

        tracing::info!("Database connection established");

        Ok(Self { pool, repository })
    }
}
