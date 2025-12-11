use axum::{extract::State, Json};
use chrono::Utc;
use serde::Serialize;
use sqlx::Row;

use crate::commands::serve::{middleware::AppError, models::DataResponse, state::AppState};

#[derive(Debug, Serialize)]
pub struct SymbolsResponse {
    pub symbols: Vec<String>,
    pub count: usize,
}

/// GET /api/v1/symbols
/// Get list of all supported symbols from the tickers table
pub async fn get_symbols(
    State(state): State<AppState>,
) -> Result<Json<DataResponse<SymbolsResponse>>, AppError> {
    let query_str = r#"
        SELECT DISTINCT symbol
        FROM tickers
        ORDER BY symbol ASC
    "#;

    let rows = sqlx::query(query_str)
        .fetch_all(&state.pool)
        .await
        .map_err(|e| AppError::database(e.to_string()))?;

    let symbols: Vec<String> = rows
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("symbol").ok())
        .collect();

    let count = symbols.len();

    Ok(Json(DataResponse {
        data: SymbolsResponse { symbols, count },
        timestamp: Utc::now(),
    }))
}
