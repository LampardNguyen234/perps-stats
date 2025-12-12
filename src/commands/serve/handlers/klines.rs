use axum::{
    extract::{Query, State},
    Json,
};

use crate::commands::serve::{
    middleware::AppError,
    models::{KlineQuery, PaginatedResponse, PaginationMeta},
    state::AppState,
};
use perps_core::Kline;
use perps_database::Repository;

/// GET /api/v1/klines
/// Get OHLCV candlestick data
#[allow(dead_code)]
pub async fn get_klines(
    State(state): State<AppState>,
    Query(query): Query<KlineQuery>,
) -> Result<Json<PaginatedResponse<Kline>>, AppError> {
    let start = query.start_datetime();
    let end = query.end_datetime();
    let limit = query.validated_limit();

    let klines = state
        .repository
        .get_klines(
            &query.exchange,
            &query.symbol,
            &query.interval,
            start,
            end,
            Some(limit),
        )
        .await
        .map_err(|e| AppError::database(e.to_string()))?;

    Ok(Json(PaginatedResponse {
        pagination: PaginationMeta {
            total: None,
            limit,
            offset: 0,
            count: klines.len(),
        },
        data: klines,
    }))
}
