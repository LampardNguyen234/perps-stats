mod handlers;
pub mod middleware;
mod models;
pub mod rate_limiter;
pub mod routes;
pub mod state;

use anyhow::Result;
use axum::middleware as axum_middleware;
use std::time::Duration;
use std::net::SocketAddr;
use tower::ServiceBuilder;
use tower_http::{
    cors::{Any, CorsLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::commands::serve::{routes::create_routes, state::AppState};

/// Execute the serve command - start the REST API server
pub async fn execute(host: String, port: u16) -> Result<()> {
    tracing::info!("Starting API server on {}:{}", host, port);

    // Get database URL from environment
    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL environment variable not set"))?;

    // Initialize application state (includes rate limiter with default config)
    let state = AppState::new(&database_url).await?;
    let rate_limiter = state.rate_limiter.clone();

    // Create router with all routes
    let app = create_routes(state);

    // Add middleware layers with rate limiting first
    let app = app
        // Add rate limiting middleware
        .layer(axum_middleware::from_fn_with_state(
            rate_limiter.clone(),
            middleware::rate_limit::rate_limit_check_state,
        ))
        .layer(
            ServiceBuilder::new()
                // Logging layer for request/response tracing
                .layer(TraceLayer::new_for_http())
                // CORS layer - allow all origins for development
                // TODO: Make this configurable for production
                .layer(
                    CorsLayer::new()
                        .allow_origin(Any)
                        .allow_methods(Any)
                        .allow_headers(Any),
                )
                // Timeout layer - 30 second timeout for requests
                .layer(TimeoutLayer::new(Duration::from_secs(30))),
        );

    // Bind to address
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("API server listening on http://{}", addr);
    tracing::info!("Health check available at: http://{}/api/v1/health", addr);
    tracing::info!("Database stats available at: http://{}/api/v1/stats", addr);
    tracing::info!("Rate limiting enabled: 2 req/sec per IP, 100 req/sec global");

    // Start server with ConnectInfo support
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
