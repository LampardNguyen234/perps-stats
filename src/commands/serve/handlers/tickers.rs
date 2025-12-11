use axum::{extract::{Query, State}, Json};
use chrono::Utc;
use sqlx::Row;

use crate::commands::serve::{
    middleware::AppError,
    models::{DataResponse, PaginatedResponse, PaginationMeta, TimeRangeQuery, TickerWithExchange},
    state::AppState,
};
use perps_core::Ticker;
use perps_database::Repository;

/// GET /api/v1/tickers
/// Get latest ticker for a symbol
/// - If exchange provided: returns ticker for that specific exchange/symbol combination
/// - If exchange not provided: returns latest ticker from each exchange for that symbol
pub async fn get_latest_tickers(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<DataResponse<Vec<TickerWithExchange>>>, AppError> {
    // Validate query parameters
    query.validate()?;

    match &query.exchange {
        // Exchange provided: get specific exchange/symbol
        Some(exchange) => {
            let ticker = state
                .repository
                .get_latest_ticker(exchange, &query.symbol)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let result = ticker.map(|t| vec![TickerWithExchange {
                exchange: exchange.clone(),
                ticker: t,
            }]).unwrap_or_default();

            Ok(Json(DataResponse {
                data: result,
                timestamp: Utc::now(),
            }))
        }
        // No exchange: get latest from each exchange for this symbol
        None => {
            let query_str = r#"
                SELECT DISTINCT ON (e.name)
                       e.name as exchange_name,
                       t.last_price, t.mark_price, t.index_price,
                       t.best_bid_price, t.best_bid_qty, t.best_ask_price, t.best_ask_qty,
                       t.volume_24h, t.turnover_24h, t.open_interest, t.open_interest_notional,
                       t.price_change_24h, t.price_change_pct,
                       t.high_24h, t.low_24h, t.symbol, t.ts
                FROM tickers t
                JOIN exchanges e ON t.exchange_id = e.id
                WHERE t.symbol = $1
                ORDER BY e.name, t.ts DESC
            "#;

            let rows = sqlx::query(query_str)
                .bind(&query.normalized_symbol())
                .fetch_all(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let result: Vec<TickerWithExchange> = rows.into_iter().map(|r| TickerWithExchange {
                exchange: r.get("exchange_name"),
                ticker: Ticker {
                    symbol: r.get("symbol"),
                    last_price: r.get("last_price"),
                    mark_price: r.get("mark_price"),
                    index_price: r.get("index_price"),
                    best_bid_price: r.get("best_bid_price"),
                    best_bid_qty: r.try_get("best_bid_qty").unwrap_or(rust_decimal::Decimal::ZERO),
                    best_ask_price: r.get("best_ask_price"),
                    best_ask_qty: r.try_get("best_ask_qty").unwrap_or(rust_decimal::Decimal::ZERO),
                    volume_24h: r.get("volume_24h"),
                    turnover_24h: r.get("turnover_24h"),
                    open_interest: r.try_get("open_interest").unwrap_or(rust_decimal::Decimal::ZERO),
                    open_interest_notional: r.try_get("open_interest_notional").unwrap_or(rust_decimal::Decimal::ZERO),
                    price_change_24h: r.get("price_change_24h"),
                    price_change_pct: r.try_get("price_change_pct").unwrap_or(rust_decimal::Decimal::ZERO),
                    high_price_24h: r.get("high_24h"),
                    low_price_24h: r.get("low_24h"),
                    timestamp: r.get("ts"),
                },
            }).collect();

            Ok(Json(DataResponse {
                data: result,
                timestamp: Utc::now(),
            }))
        }
    }
}

/// GET /api/v1/tickers/history
/// Get historical ticker data within a time range for a symbol
/// - If exchange provided: returns tickers for that specific exchange/symbol combination
/// - If exchange not provided: returns tickers for that symbol across all exchanges
pub async fn get_ticker_history(
    State(state): State<AppState>,
    Query(query): Query<TimeRangeQuery>,
) -> Result<Json<PaginatedResponse<TickerWithExchange>>, AppError> {
    // Validate query parameters
    query.validate()?;

    let start = query.start_datetime();
    let end = query.end_datetime();
    let limit = query.validated_limit();

    // Get total count and data
    let (total_count, tickers): (i64, Vec<TickerWithExchange>) = match &query.exchange {
        // Exchange provided: use existing repository method
        Some(exchange) => {
            // Get total count first
            let count_query = r#"
                SELECT COUNT(*) 
                FROM tickers t
                JOIN exchanges e ON t.exchange_id = e.id
                WHERE e.name = $1 AND t.symbol = $2
                  AND t.ts >= $3 AND t.ts <= $4
                "#;
            
            let total: (i64,) = sqlx::query_as(count_query)
                .bind(exchange)
                .bind(&query.normalized_symbol())
                .bind(start)
                .bind(end)
                .fetch_one(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let ticker_data = state
                .repository
                .get_tickers(exchange, &query.normalized_symbol(), start, end, Some(limit))
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let tickers = ticker_data.into_iter().map(|t| TickerWithExchange {
                exchange: exchange.clone(),
                ticker: t,
            }).collect();

            (total.0, tickers)
        }
        // No exchange: get all exchanges for this symbol
        None => {
            // Get total count first
            let count_query = r#"
                SELECT COUNT(*)
                FROM tickers t
                JOIN exchanges e ON t.exchange_id = e.id
                WHERE t.symbol = $1
                  AND t.ts >= $2 AND t.ts <= $3
                "#;
            
            let total: (i64,) = sqlx::query_as(count_query)
                .bind(&query.normalized_symbol())
                .bind(start)
                .bind(end)
                .fetch_one(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let query_str = r#"
                SELECT e.name as exchange_name,
                       t.last_price, t.mark_price, t.index_price,
                       t.best_bid_price, t.best_bid_qty, t.best_ask_price, t.best_ask_qty,
                       t.volume_24h, t.turnover_24h, t.open_interest, t.open_interest_notional,
                       t.price_change_24h, t.price_change_pct,
                       t.high_24h, t.low_24h, t.symbol, t.ts
                FROM tickers t
                JOIN exchanges e ON t.exchange_id = e.id
                WHERE t.symbol = $1
                  AND t.ts >= $2 AND t.ts <= $3
                ORDER BY t.ts DESC
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

            let tickers = rows.into_iter().map(|r| TickerWithExchange {
                exchange: r.get("exchange_name"),
                ticker: Ticker {
                    symbol: r.get("symbol"),
                    last_price: r.get("last_price"),
                    mark_price: r.get("mark_price"),
                    index_price: r.get("index_price"),
                    best_bid_price: r.get("best_bid_price"),
                    best_bid_qty: r.try_get("best_bid_qty").unwrap_or(rust_decimal::Decimal::ZERO),
                    best_ask_price: r.get("best_ask_price"),
                    best_ask_qty: r.try_get("best_ask_qty").unwrap_or(rust_decimal::Decimal::ZERO),
                    volume_24h: r.get("volume_24h"),
                    turnover_24h: r.get("turnover_24h"),
                    open_interest: r.try_get("open_interest").unwrap_or(rust_decimal::Decimal::ZERO),
                    open_interest_notional: r.try_get("open_interest_notional").unwrap_or(rust_decimal::Decimal::ZERO),
                    price_change_24h: r.get("price_change_24h"),
                    price_change_pct: r.try_get("price_change_pct").unwrap_or(rust_decimal::Decimal::ZERO),
                    high_price_24h: r.get("high_24h"),
                    low_price_24h: r.get("low_24h"),
                    timestamp: r.get("ts"),
                },
            }).collect();

            (total.0, tickers)
        }
    };

    Ok(Json(PaginatedResponse {
        pagination: PaginationMeta {
            total: Some(total_count),
            limit,
            offset: query.offset,
            count: tickers.len(),
        },
        data: tickers,
    }))
}
