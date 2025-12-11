use axum::{routing::get, Router};
use tower_http::services::ServeDir;

use crate::commands::serve::{handlers, state::AppState};

/// Create the main API router with all endpoints
pub fn create_routes(state: AppState) -> Router {
    // Serve static files from src/commands/serve/api directory
    let serve_dir = ServeDir::new("src/commands/serve/api");

    Router::new()
        // Health & metadata
        .route("/api/v1/health", get(handlers::health::health_check))
        .route("/api/v1/stats", get(handlers::health::database_stats))
        .route("/api/v1/exchanges", get(handlers::exchanges::list_exchanges))
        .route("/api/v1/symbols", get(handlers::symbols::get_symbols))
        // Tickers
        .route("/api/v1/tickers", get(handlers::tickers::get_latest_tickers))
        .route(
            "/api/v1/tickers/history",
            get(handlers::tickers::get_ticker_history),
        )
        // Liquidity
        .route(
            "/api/v1/liquidity",
            get(handlers::liquidity::get_latest_liquidity),
        )
        .route(
            "/api/v1/liquidity/history",
            get(handlers::liquidity::get_liquidity_history),
        )
        // Klines - DISABLED (no data yet)
        // .route("/api/v1/klines", get(handlers::klines::get_klines))
        // Funding rates - DISABLED (no data yet)
        // .route(
        //     "/api/v1/funding-rates",
        //     get(handlers::funding_rates::get_latest_funding_rates),
        // )
        // .route(
        //     "/api/v1/funding-rates/history",
        //     get(handlers::funding_rates::get_funding_rate_history),
        // )
        // Trades
        // .route("/api/v1/trades", get(handlers::trades::get_trades))
        // // Orderbooks
        .route(
            "/api/v1/orderbooks",
            get(handlers::orderbooks::get_latest_orderbooks),
        )
        .route(
            "/api/v1/orderbooks/history",
            get(handlers::orderbooks::get_orderbook_history),
        )
        // Slippage endpoints
        .route("/api/v1/slippage", get(handlers::slippage::get_latest_slippage))
        .route(
            "/api/v1/slippage/history",
            get(handlers::slippage::get_slippage_history),
        )
        // Static files for interactive API docs (serve at /api/*)
        .nest_service("/api", serve_dir)
        .with_state(state)
}
