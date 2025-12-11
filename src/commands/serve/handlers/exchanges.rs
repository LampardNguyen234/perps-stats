use axum::{extract::State, Json};
use rust_decimal::prelude::ToPrimitive;

use crate::commands::serve::{middleware::AppError, models::ExchangeInfo, state::AppState};

/// GET /api/v1/exchanges
/// List all exchanges from the database
pub async fn list_exchanges(
    State(state): State<AppState>,
) -> Result<Json<Vec<ExchangeInfo>>, AppError> {
    let exchanges: Vec<(i32, String, Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>)> =
        sqlx::query_as("SELECT id, name, maker_fee, taker_fee FROM exchanges ORDER BY name")
            .fetch_all(&state.pool)
            .await?;

    let exchange_info: Vec<ExchangeInfo> = exchanges
        .into_iter()
        .map(|(id, name, maker_fee, taker_fee)| ExchangeInfo {
            id,
            name,
            maker_fee: maker_fee.and_then(|d| d.to_f64()),
            taker_fee: taker_fee.and_then(|d| d.to_f64()),
        })
        .collect();

    Ok(Json(exchange_info))
}
