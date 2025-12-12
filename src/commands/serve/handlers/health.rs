use axum::{extract::State, Json};
use chrono::Utc;

use crate::commands::serve::{
    middleware::AppError,
    models::responses::{HealthResponse, StatsResponse},
    state::AppState,
};

/// GET /api/v1/health
/// Health check endpoint - verifies database connectivity
pub async fn health_check(State(state): State<AppState>) -> Result<Json<HealthResponse>, AppError> {
    // Test database connection
    let db_status = match sqlx::query("SELECT 1").fetch_one(&state.pool).await {
        Ok(_) => "healthy",
        Err(_) => "unhealthy",
    };

    Ok(Json(HealthResponse {
        status: if db_status == "healthy" {
            "ok".to_string()
        } else {
            "degraded".to_string()
        },
        database: db_status.to_string(),
        timestamp: Utc::now(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }))
}

/// GET /api/v1/stats
/// Database statistics endpoint
pub async fn database_stats(
    State(state): State<AppState>,
) -> Result<Json<StatsResponse>, AppError> {
    // Count records in each table
    let total_tickers: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tickers")
        .fetch_one(&state.pool)
        .await?;

    let total_liquidity: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM liquidity_depth")
        .fetch_one(&state.pool)
        .await?;

    let total_orderbooks: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM orderbooks")
        .fetch_one(&state.pool)
        .await?;

    let total_trades: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM trades")
        .fetch_one(&state.pool)
        .await?;

    let total_klines: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM klines")
        .fetch_one(&state.pool)
        .await?;

    let total_funding: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM funding_rates")
        .fetch_one(&state.pool)
        .await?;

    // Get oldest and newest timestamps from tickers table
    let oldest: (Option<chrono::DateTime<Utc>>,) = sqlx::query_as("SELECT MIN(ts) FROM tickers")
        .fetch_one(&state.pool)
        .await?;

    let newest: (Option<chrono::DateTime<Utc>>,) = sqlx::query_as("SELECT MAX(ts) FROM tickers")
        .fetch_one(&state.pool)
        .await?;

    Ok(Json(StatsResponse {
        total_tickers: total_tickers.0,
        total_liquidity_depth: total_liquidity.0,
        total_orderbooks: total_orderbooks.0,
        total_trades: total_trades.0,
        total_klines: total_klines.0,
        total_funding_rates: total_funding.0,
        oldest_data: oldest.0,
        newest_data: newest.0,
    }))
}
