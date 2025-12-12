mod handlers;
mod middleware;
mod models;
pub mod routes;
pub mod state;

use anyhow::Result;
use std::time::Duration;
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

    // Initialize application state
    let state = AppState::new(&database_url).await?;

    // Create router with all routes
    let app = create_routes(state);

    // Add middleware layers
    let app = app.layer(
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

    // Start server
    axum::serve(listener, app).await?;

    Ok(())
}
