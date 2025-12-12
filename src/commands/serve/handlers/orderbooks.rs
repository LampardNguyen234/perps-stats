use axum::{
    extract::{Query, State},
    Json,
};
use chrono::Utc;
use sqlx::Row;

use crate::commands::serve::{
    middleware::AppError,
    models::{
        DataResponse, OrderbookWithExchange, PaginatedResponse, PaginationMeta, TimeRangeQuery,
    },
    state::AppState,
};
use perps_core::Orderbook;
use perps_database::Repository;

/// GET /api/v1/orderbooks
/// Get latest orderbook for a symbol
/// - If exchange provided: returns orderbook for that specific exchange/symbol combination
/// - If exchange not provided: returns latest orderbook from each exchange for that symbol
pub async fn get_latest_orderbooks(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<DataResponse<Vec<OrderbookWithExchange>>>, AppError> {
    // Validate query parameters
    query.validate()?;

    match &query.exchange {
        // Exchange provided: use existing logic
        Some(exchange) => {
            let orderbook = state
                .repository
                .get_latest_orderbook(exchange, &query.symbol)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let result = orderbook
                .map(|ob| {
                    vec![OrderbookWithExchange {
                        exchange: exchange.clone(),
                        orderbook: ob,
                    }]
                })
                .unwrap_or_default();

            Ok(Json(DataResponse {
                data: result,
                timestamp: Utc::now(),
            }))
        }
        // No exchange: get latest from each exchange for this symbol
        None => {
            let query_str = r#"
                SELECT DISTINCT ON (e.name)
                       e.name as exchange,
                       ob.raw_book, ob.ts
                FROM orderbooks ob
                INNER JOIN exchanges e ON ob.exchange_id = e.id
                WHERE ob.symbol = $1
                ORDER BY e.name, ob.ts DESC
            "#;

            let rows = sqlx::query(query_str)
                .bind(query.normalized_symbol())
                .fetch_all(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let result: Vec<OrderbookWithExchange> = rows
                .into_iter()
                .filter_map(|r| {
                    let exchange: String = r.get("exchange");
                    let raw_book: serde_json::Value = r.get("raw_book");
                    serde_json::from_value::<Orderbook>(raw_book)
                        .ok()
                        .map(|ob| OrderbookWithExchange {
                            exchange,
                            orderbook: ob,
                        })
                })
                .collect();

            Ok(Json(DataResponse {
                data: result,
                timestamp: Utc::now(),
            }))
        }
    }
}

/// GET /api/v1/orderbooks/history
/// Get historical orderbook snapshots within a time range for a symbol
/// - If exchange provided: returns orderbooks for that specific exchange/symbol combination
/// - If exchange not provided: returns orderbooks for that symbol across all exchanges
pub async fn get_orderbook_history(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<PaginatedResponse<OrderbookWithExchange>>, AppError> {
    // Validate query parameters
    query.validate()?;

    let start = query.start_datetime();
    let end = query.end_datetime();
    let limit = query.validated_limit();

    let orderbooks: Vec<OrderbookWithExchange> = match &query.exchange {
        // Exchange provided: use existing repository method
        Some(exchange) => {
            let orderbook_data = state
                .repository
                .get_orderbooks(
                    exchange,
                    &query.normalized_symbol(),
                    start,
                    end,
                    Some(limit),
                )
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            orderbook_data
                .into_iter()
                .map(|ob| OrderbookWithExchange {
                    exchange: exchange.clone(),
                    orderbook: ob,
                })
                .collect()
        }
        // No exchange: get all exchanges for this symbol
        None => {
            let query_str = r#"
                SELECT e.name as exchange, ob.raw_book, ob.ts
                FROM orderbooks ob
                JOIN exchanges e ON ob.exchange_id = e.id
                WHERE ob.symbol = $1
                  AND ob.ts >= $2 AND ob.ts <= $3
                ORDER BY ob.ts DESC
                LIMIT $4
                "#;

            let rows = sqlx::query(query_str)
                .bind(&query.symbol)
                .bind(start)
                .bind(end)
                .bind(limit)
                .fetch_all(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            rows.into_iter()
                .filter_map(|row| {
                    let exchange: String = row.get("exchange");
                    let raw_book: serde_json::Value = row.get("raw_book");
                    serde_json::from_value::<Orderbook>(raw_book)
                        .ok()
                        .map(|ob| OrderbookWithExchange {
                            exchange,
                            orderbook: ob,
                        })
                })
                .collect()
        }
    };

    Ok(Json(PaginatedResponse {
        pagination: PaginationMeta {
            total: None,
            limit,
            offset: query.offset,
            count: orderbooks.len(),
        },
        data: orderbooks,
    }))
}
