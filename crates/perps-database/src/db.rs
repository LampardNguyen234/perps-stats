use anyhow::{Context, Result};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::str::FromStr;
use std::time::Duration;

/// Database configuration
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://localhost/perps".to_string()),
            max_connections: 10,
            min_connections: 2,
            acquire_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
        }
    }
}

impl DatabaseConfig {
    pub fn from_url(url: String) -> Self {
        Self {
            url,
            ..Default::default()
        }
    }

    pub fn with_max_connections(mut self, max_connections: u32) -> Self {
        self.max_connections = max_connections;
        self
    }
}

/// Create a database connection pool with the given configuration
pub async fn create_pool(config: &DatabaseConfig) -> Result<PgPool> {
    let connect_options = PgConnectOptions::from_str(&config.url)
        .context("Failed to parse database URL")?;

    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(config.acquire_timeout)
        .idle_timeout(config.idle_timeout)
        .connect_with(connect_options)
        .await
        .context("Failed to create database pool")?;

    tracing::info!(
        "Database pool created with {} max connections",
        config.max_connections
    );

    Ok(pool)
}

/// Run database migrations
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    tracing::info!("Running database migrations...");

    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .context("Failed to run migrations")?;

    tracing::info!("Database migrations completed successfully");
    Ok(())
}

/// Check if the database is accessible
pub async fn health_check(pool: &PgPool) -> Result<()> {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .context("Database health check failed")?;

    Ok(())
}
