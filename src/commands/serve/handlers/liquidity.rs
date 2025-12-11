use axum::{extract::{Query, State}, Json};
use chrono::Utc;
use sqlx::Row;

use crate::commands::serve::{
    middleware::AppError,
    models::{DataResponse, PaginatedResponse, PaginationMeta, TimeRangeQuery},
    state::AppState,
};
use perps_core::LiquidityDepthStats;
use perps_database::Repository;

/// GET /api/v1/liquidity
/// Get latest liquidity depth stats for a symbol
/// - If exchange provided: returns liquidity depth for that specific exchange/symbol combination
/// - If exchange not provided: returns latest liquidity depth from each exchange for that symbol
pub async fn get_latest_liquidity(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<DataResponse<Vec<LiquidityDepthStats>>>, AppError> {
    // Validate query parameters
    query.validate()?;

    match &query.exchange {
        // Exchange provided: use existing logic
        Some(exchange) => {
            let liquidity = state
                .repository
                .get_liquidity_depth(exchange, &query.symbol, Utc::now() - chrono::Duration::hours(1), Utc::now(), Some(1))
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            Ok(Json(DataResponse {
                data: liquidity,
                timestamp: Utc::now(),
            }))
        }
        // No exchange: get latest from each exchange for this symbol
        None => {
            let query_str = r#"
                SELECT DISTINCT ON (e.name)
                       e.name as exchange,
                       ld.mid_price, ld.bid_1bps, ld.bid_2_5bps, ld.bid_5bps, ld.bid_10bps, ld.bid_20bps,
                       ld.ask_1bps, ld.ask_2_5bps, ld.ask_5bps, ld.ask_10bps, ld.ask_20bps, ld.ts,
                       ld.max_bid_bps, ld.max_ask_bps, ld.symbol
                FROM liquidity_depth ld
                INNER JOIN exchanges e ON ld.exchange_id = e.id
                WHERE ld.symbol = $1
                ORDER BY e.name, ld.ts DESC
            "#;

            let rows = sqlx::query(query_str)
                .bind(&query.normalized_symbol())
                .fetch_all(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let result: Vec<LiquidityDepthStats> = rows.into_iter().map(|r| LiquidityDepthStats {
                exchange: r.get("exchange"),
                symbol: r.get("symbol"),
                mid_price: r.get("mid_price"),
                bid_1bps: r.get("bid_1bps"),
                bid_2_5bps: r.get("bid_2_5bps"),
                bid_5bps: r.get("bid_5bps"),
                bid_10bps: r.get("bid_10bps"),
                bid_20bps: r.get("bid_20bps"),
                ask_1bps: r.get("ask_1bps"),
                ask_2_5bps: r.get("ask_2_5bps"),
                ask_5bps: r.get("ask_5bps"),
                ask_10bps: r.get("ask_10bps"),
                ask_20bps: r.get("ask_20bps"),
                timestamp: r.get("ts"),
                max_bid_bps: r.get("max_bid_bps"),
                max_ask_bps: r.get("max_ask_bps"),
            }).collect();

            Ok(Json(DataResponse {
                data: result,
                timestamp: Utc::now(),
            }))
        }
    }
}

/// GET /api/v1/liquidity/history
/// Get historical liquidity depth data within a time range for a symbol
/// - If exchange provided: returns liquidity depth for that specific exchange/symbol combination
/// - If exchange not provided: returns liquidity depth for that symbol across all exchanges
pub async fn get_liquidity_history(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<PaginatedResponse<LiquidityDepthStats>>, AppError> {
    // Validate query parameters
    query.validate()?;

    let start = query.start_datetime();
    let end = query.end_datetime();
    let limit = query.validated_limit();

    // Get total count and data
    let (total_count, liquidity): (i64, Vec<LiquidityDepthStats>) = match &query.exchange {
        // Exchange provided: use existing repository method
        Some(exchange) => {
            // Get total count first
            let count_query = r#"
                SELECT COUNT(*) 
                FROM liquidity_depth ld
                INNER JOIN exchanges e ON ld.exchange_id = e.id
                WHERE e.name = $1 AND ld.symbol = $2
                  AND ld.ts >= $3 AND ld.ts <= $4
                "#;
            
            let total: (i64,) = sqlx::query_as(count_query)
                .bind(exchange)
                .bind(&query.normalized_symbol())
                .bind(start)
                .bind(end)
                .fetch_one(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let liquidity_data = state
                .repository
                .get_liquidity_depth(exchange, &query.normalized_symbol(), start, end, Some(limit))
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            (total.0, liquidity_data)
        }
        // No exchange: get all exchanges for this symbol
        None => {
            // Get total count first
            let count_query = r#"
                SELECT COUNT(*)
                FROM liquidity_depth ld
                INNER JOIN exchanges e ON ld.exchange_id = e.id
                WHERE ld.symbol = $1
                  AND ld.ts >= $2 AND ld.ts <= $3
                "#;
            
            let total: (i64,) = sqlx::query_as(count_query)
                .bind(&query.normalized_symbol())
                .bind(start)
                .bind(end)
                .fetch_one(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let query_str = r#"
                SELECT ld.mid_price, ld.bid_1bps, ld.bid_2_5bps, ld.bid_5bps, ld.bid_10bps, ld.bid_20bps,
                       ld.ask_1bps, ld.ask_2_5bps, ld.ask_5bps, ld.ask_10bps, ld.ask_20bps, ld.ts,
                       ld.max_bid_bps, ld.max_ask_bps, ld.symbol, e.name as exchange
                FROM liquidity_depth ld
                INNER JOIN exchanges e ON ld.exchange_id = e.id
                WHERE ld.symbol = $1
                  AND ld.ts >= $2 AND ld.ts <= $3
                ORDER BY ld.ts DESC
                LIMIT $4
                OFFSET $5
                "#;

            let rows = sqlx::query(query_str)
                .bind(&query.normalized_symbol())
                .bind(start)
                .bind(end)
                .bind(limit)
                .bind(query.offset)
                .fetch_all(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let liquidity_data = rows.into_iter().map(|r| LiquidityDepthStats {
                exchange: r.get("exchange"),
                symbol: r.get("symbol"),
                mid_price: r.get("mid_price"),
                bid_1bps: r.get("bid_1bps"),
                bid_2_5bps: r.get("bid_2_5bps"),
                bid_5bps: r.get("bid_5bps"),
                bid_10bps: r.get("bid_10bps"),
                bid_20bps: r.get("bid_20bps"),
                ask_1bps: r.get("ask_1bps"),
                ask_2_5bps: r.get("ask_2_5bps"),
                ask_5bps: r.get("ask_5bps"),
                ask_10bps: r.get("ask_10bps"),
                ask_20bps: r.get("ask_20bps"),
                timestamp: r.get("ts"),
                max_bid_bps: r.get("max_bid_bps"),
                max_ask_bps: r.get("max_ask_bps"),
            }).collect();

            (total.0, liquidity_data)
        }
    };

    Ok(Json(PaginatedResponse {
        pagination: PaginationMeta {
            total: Some(total_count),
            limit,
            offset: query.offset,
            count: liquidity.len(),
        },
        data: liquidity,
    }))
}
