use axum::{extract::{Query, State}, Json};
use chrono::Utc;

use crate::commands::serve::{
    middleware::AppError,
    models::{DataResponse, PaginatedResponse, PaginationMeta, TimeRangeQuery},
    state::AppState,
};
use perps_core::FundingRate;
use perps_database::Repository;

/// GET /api/v1/funding-rates
/// Get latest funding rate for a specific exchange/symbol
pub async fn get_latest_funding_rates(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<DataResponse<Option<FundingRate>>>, AppError> {
    let exchange = query
        .exchange
        .clone()
        .ok_or_else(|| AppError::bad_request("exchange parameter is required"))?;

    let funding_rate = state
        .repository
        .get_latest_funding_rate(&exchange, &query.symbol)
        .await
        .map_err(|e| AppError::database(e.to_string()))?;

    Ok(Json(DataResponse {
        data: funding_rate,
        timestamp: Utc::now(),
    }))
}

/// GET /api/v1/funding-rates/history
/// Get historical funding rate data within a time range
pub async fn get_funding_rate_history(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<PaginatedResponse<FundingRate>>, AppError> {
    let exchange = query
        .exchange
        .clone()
        .ok_or_else(|| AppError::bad_request("exchange parameter is required"))?;

    let start = query.start_datetime();
    let end = query.end_datetime();
    let limit = query.validated_limit();

    let funding_rates = state
        .repository
        .get_funding_rates(&exchange, &query.symbol, start, end, Some(limit))
        .await
        .map_err(|e| AppError::database(e.to_string()))?;

    Ok(Json(PaginatedResponse {
        pagination: PaginationMeta {
            total: None,
            limit,
            offset: query.offset,
            count: funding_rates.len(),
        },
        data: funding_rates,
    }))
}
