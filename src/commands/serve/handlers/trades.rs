use axum::{
    extract::{Query, State},
    Json,
};

use crate::commands::serve::{
    middleware::AppError,
    models::{PaginatedResponse, PaginationMeta, TimeRangeQuery},
    state::AppState,
};
use perps_core::Trade;
use perps_database::Repository;

/// GET /api/v1/trades
/// Get recent trades for a specific exchange/symbol
#[allow(dead_code)]
pub async fn get_trades(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<PaginatedResponse<Trade>>, AppError> {
    let exchange = query
        .exchange
        .clone()
        .ok_or_else(|| AppError::bad_request("exchange parameter is required"))?;

    let start = query.start_datetime();
    let end = query.end_datetime();
    let limit = query.validated_limit();

    let trades = state
        .repository
        .get_trades(&exchange, &query.symbol, start, end, Some(limit))
        .await
        .map_err(|e| AppError::database(e.to_string()))?;

    Ok(Json(PaginatedResponse {
        pagination: PaginationMeta {
            total: None,
            limit,
            offset: query.offset,
            count: trades.len(),
        },
        data: trades,
    }))
}
