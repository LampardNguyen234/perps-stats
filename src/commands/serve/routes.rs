use axum::{response::Html, routing::get, Router};
use tower_http::services::ServeDir;

use crate::commands::serve::{handlers, state::AppState};

/// Create the main API router with all endpoints
pub fn create_routes(state: AppState) -> Router {
    // Serve static files from src/api directory
    let serve_dir = ServeDir::new("src/api");

    // Build the /api/v1 subrouter with all API endpoints
    let api_v1_router = Router::new()
        // Health & metadata
        .route("/health", get(handlers::health::health_check))
        .route("/stats", get(handlers::health::database_stats))
        .route("/exchanges", get(handlers::exchanges::list_exchanges))
        .route("/symbols", get(handlers::symbols::get_symbols))
        // Tickers
        .route("/tickers", get(handlers::tickers::get_latest_tickers))
        .route("/tickers/history", get(handlers::tickers::get_ticker_history))
        // Liquidity
        .route("/liquidity", get(handlers::liquidity::get_latest_liquidity))
        .route(
            "/liquidity/history",
            get(handlers::liquidity::get_liquidity_history),
        )
        // Klines - DISABLED (no data yet)
        // .route("/klines", get(handlers::klines::get_klines))
        // Funding rates - DISABLED (no data yet)
        // .route("/funding-rates", get(handlers::funding_rates::get_latest_funding_rates))
        // .route(
        //     "/funding-rates/history",
        //     get(handlers::funding_rates::get_funding_rate_history),
        // )
        // Trades
        // .route("/trades", get(handlers::trades::get_trades))
        // Orderbooks
        .route("/orderbooks", get(handlers::orderbooks::get_latest_orderbooks))
        .route(
            "/orderbooks/history",
            get(handlers::orderbooks::get_orderbook_history),
        )
        // Slippage endpoints
        .route("/slippage", get(handlers::slippage::get_latest_slippage))
        .route("/slippage/history", get(handlers::slippage::get_slippage_history))
        .with_state(state.clone());

    Router::new()
        // Interactive API documentation at root /api paths
        .route("/api", get(serve_api_docs))
        .route("/api/", get(serve_api_docs))
        // Nest v1 API routes
        .nest("/api/v1", api_v1_router)
        // Static files (for CSS, JS, etc. if they exist)
        .nest_service("/api/static", serve_dir)
}

/// Serve the interactive API documentation page
async fn serve_api_docs() -> Html<&'static str> {
    Html(include_str!("../../api/index.html"))
}
