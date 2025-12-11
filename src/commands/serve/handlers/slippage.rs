use axum::{
    extract::{Query, State},
    response::Json,
};
use chrono::Utc;
use perps_core::Slippage;
use perps_database::Repository;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize)]
pub struct SlippageWithExchange {
    pub exchange: String,
    #[serde(flatten)]
    pub slippage: Slippage,
}
use crate::commands::serve::{
    middleware::error::AppError,
    models::{
        requests::TimeRangeQuery,
        responses::{DataResponse, PaginatedResponse, PaginationMeta},
    },
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct SlippageQuery {
    pub exchange: Option<String>,
    pub symbol: Option<String>, // Make optional for better error handling
    pub amount: Option<String>, // Make optional for better error handling
}

#[derive(Debug, Deserialize)]
pub struct SlippageHistoryQuery {
    pub exchange: Option<String>,
    pub symbol: Option<String>, // Make optional for better error handling
    pub amount: Option<String>, // Make optional for better error handling
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl SlippageQuery {
    /// Validate required parameters
    pub fn validate(&self) -> Result<(), AppError> {
        // Validate symbol is provided
        let symbol = match &self.symbol {
            Some(s) => s,
            None => return Err(AppError::bad_request("Symbol parameter is required".to_string())),
        };

        // Validate symbol is not empty
        if symbol.trim().is_empty() {
            return Err(AppError::bad_request("Symbol parameter is required and cannot be empty".to_string()));
        }

        // Validate symbol format (alphanumeric, max 20 chars)
        if !symbol.chars().all(|c| c.is_alphanumeric()) || symbol.len() > 20 {
            return Err(AppError::bad_request("Symbol must be alphanumeric and max 20 characters".to_string()));
        }

        // Validate exchange if provided
        if let Some(exchange) = &self.exchange {
            if exchange.trim().is_empty() {
                return Err(AppError::bad_request("Exchange parameter cannot be empty".to_string()));
            }
            if !exchange.chars().all(|c| c.is_alphanumeric() || c == '_') || exchange.len() > 50 {
                return Err(AppError::bad_request("Exchange must be alphanumeric (with underscores) and max 50 characters".to_string()));
            }
        }

        // Validate amount is provided
        let amount = match &self.amount {
            Some(a) => a,
            None => return Err(AppError::bad_request("Amount parameter is required".to_string())),
        };

        // Validate amount is not empty
        if amount.trim().is_empty() {
            return Err(AppError::bad_request("Amount parameter is required and cannot be empty".to_string()));
        }

        // Validate amount format (basic check for numeric with optional decimal)
        if !amount.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err(AppError::bad_request("Amount must be a valid number".to_string()));
        }

        Ok(())
    }

    /// Get symbol (assumes validation has passed)
    pub fn symbol(&self) -> &str {
        self.symbol.as_ref().unwrap()
    }

    /// Get amount (assumes validation has passed)
    pub fn amount(&self) -> &str {
        self.amount.as_ref().unwrap()
    }
}

impl SlippageHistoryQuery {
    /// Validate required parameters
    pub fn validate(&self) -> Result<(), AppError> {
        // Validate symbol is provided
        let symbol = match &self.symbol {
            Some(s) => s,
            None => return Err(AppError::bad_request("Symbol parameter is required".to_string())),
        };

        // Validate symbol is not empty
        if symbol.trim().is_empty() {
            return Err(AppError::bad_request("Symbol parameter is required and cannot be empty".to_string()));
        }

        // Validate symbol format (alphanumeric, max 20 chars)
        if !symbol.chars().all(|c| c.is_alphanumeric()) || symbol.len() > 20 {
            return Err(AppError::bad_request("Symbol must be alphanumeric and max 20 characters".to_string()));
        }

        // Validate exchange if provided
        if let Some(exchange) = &self.exchange {
            if exchange.trim().is_empty() {
                return Err(AppError::bad_request("Exchange parameter cannot be empty".to_string()));
            }
            if !exchange.chars().all(|c| c.is_alphanumeric() || c == '_') || exchange.len() > 50 {
                return Err(AppError::bad_request("Exchange must be alphanumeric (with underscores) and max 50 characters".to_string()));
            }
        }

        // Validate amount is provided
        let amount = match &self.amount {
            Some(a) => a,
            None => return Err(AppError::bad_request("Amount parameter is required".to_string())),
        };

        // Validate amount is not empty
        if amount.trim().is_empty() {
            return Err(AppError::bad_request("Amount parameter is required and cannot be empty".to_string()));
        }

        // Validate amount format (basic check for numeric with optional decimal)
        if !amount.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err(AppError::bad_request("Amount must be a valid number".to_string()));
        }

        // Validate limit if provided
        if let Some(limit) = self.limit {
            if limit < 1 || limit > 1000 {
                return Err(AppError::bad_request("Limit must be between 1 and 1000".to_string()));
            }
        }

        // Validate offset if provided
        if let Some(offset) = self.offset {
            if offset < 0 {
                return Err(AppError::bad_request("Offset must be non-negative".to_string()));
            }
        }

        // Validate timestamp range
        if let (Some(start), Some(end)) = (self.start, self.end) {
            if start > end {
                return Err(AppError::bad_request("Start timestamp must be before end timestamp".to_string()));
            }
        }

        Ok(())
    }

    /// Get symbol (assumes validation has passed)
    pub fn symbol(&self) -> &str {
        self.symbol.as_ref().unwrap()
    }

    /// Get amount (assumes validation has passed)
    pub fn amount(&self) -> &str {
        self.amount.as_ref().unwrap()
    }

    /// Get validated limit with default and cap
    pub fn validated_limit(&self) -> i64 {
        self.limit.unwrap_or(100).min(500).max(1)
    }

    /// Get start datetime with default (24 hours ago)
    pub fn start_datetime(&self) -> chrono::DateTime<Utc> {
        match self.start {
            Some(timestamp) => chrono::DateTime::from_timestamp(timestamp, 0)
                .unwrap_or_else(|| Utc::now() - chrono::Duration::hours(24)),
            None => Utc::now() - chrono::Duration::hours(24),
        }
    }

    /// Get end datetime with default (now)
    pub fn end_datetime(&self) -> chrono::DateTime<Utc> {
        match self.end {
            Some(timestamp) => chrono::DateTime::from_timestamp(timestamp, 0)
                .unwrap_or_else(|| Utc::now()),
            None => Utc::now(),
        }
    }

    /// Get validated offset
    pub fn validated_offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

/// Filter slippages based on amount and side criteria
fn filter_slippages(slippages: Vec<Slippage>, amount_filter: Option<Decimal>, side_filter: Option<&str>) -> Vec<Slippage> {
    slippages.into_iter().filter(|slippage| {
        // Filter by trade amount if specified
        if let Some(target_amount) = amount_filter {
            if slippage.trade_amount != target_amount {
                return false;
            }
        }

        // Filter by side feasibility if specified
        if let Some(side) = side_filter {
            match side.to_lowercase().as_str() {
                "ask" => {
                    if !slippage.buy_feasible {
                        return false;
                    }
                },
                "bid" => {
                    if !slippage.sell_feasible {
                        return false;
                    }
                },
                _ => return false, // Invalid side
            }
        }

        true
    }).collect()
}

/// Filter slippages with exchange based on amount criteria
fn filter_slippages_with_exchange(slippages: Vec<SlippageWithExchange>, amount_filter: Decimal) -> Vec<SlippageWithExchange> {
    slippages.into_iter().filter(|slippage_with_exchange| {
        let slippage = &slippage_with_exchange.slippage;
        
        // Filter by trade amount
        slippage.trade_amount == amount_filter
    }).collect()
}

/// Build SQL WHERE conditions for amount filter
fn build_sql_filters(amount_filter: Decimal, base_param_count: usize) -> (String, Vec<Decimal>) {
    let param_counter = base_param_count + 1;
    let where_clause = format!(" AND s.trade_amount = ${}", param_counter);
    let params = vec![amount_filter];

    (where_clause, params)
}

/// Get latest slippage for a symbol
/// 
/// Returns the most recent slippage calculations for a symbol.
/// If exchange is specified: returns slippage for that specific exchange/symbol combination (single result).
/// If exchange is omitted: returns latest slippage from each exchange for the symbol (multiple results).
/// Optional amount and side filters can be applied.
pub async fn get_latest_slippage(
    State(state): State<AppState>,
    Query(query): Query<SlippageQuery>,
) -> Result<Json<DataResponse<Vec<SlippageWithExchange>>>, AppError> {
    // Validate query parameters
    query.validate()?;

    // Parse required amount parameter
    let amount_filter = match Decimal::from_str(query.amount().trim()) {
        Ok(amount) => amount,
        Err(_) => return Err(AppError::bad_request("Invalid amount parameter".to_string())),
    };

    let mut slippages = match &query.exchange {
        // Exchange provided: use existing repository method
        Some(exchange) => {
            let slippage_data = state
                .repository
                .get_latest_slippage(exchange, &query.symbol())
                .await
                .map_err(|e| AppError::database(e.to_string()))?
                .unwrap_or_default();

            // Convert each slippage to SlippageWithExchange
            slippage_data.into_iter().map(|s| SlippageWithExchange {
                exchange: exchange.clone(),
                slippage: s,
            }).collect()
        }
        // No exchange: get all exchanges for this symbol
        None => {
            let (where_clause, params) = build_sql_filters(amount_filter, 1); // 1 because $1 is symbol
            let query_str = format!(r#"
                SELECT DISTINCT ON (e.name) e.name as exchange_name,
                       s.symbol, s.mid_price, s.trade_amount,
                       s.buy_avg_price, s.buy_slippage_bps, s.buy_slippage_pct, s.buy_total_cost, s.buy_feasible,
                       s.sell_avg_price, s.sell_slippage_bps, s.sell_slippage_pct, s.sell_total_cost, s.sell_feasible,
                       s.ts
                FROM slippage s
                JOIN exchanges e ON s.exchange_id = e.id
                WHERE s.symbol = $1{}
                ORDER BY e.name, s.ts DESC
                "#, where_clause);

            let symbol = query.symbol();
            let mut sql_query = sqlx::query(&query_str).bind(&symbol);
            
            // Bind additional parameters for filters
            for param in params {
                sql_query = sql_query.bind(param);
            }
            
            let rows = sql_query
                .fetch_all(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            rows.into_iter().map(|r| SlippageWithExchange {
                exchange: r.get("exchange_name"),
                slippage: Slippage {
                    symbol: r.get("symbol"),
                    timestamp: r.get("ts"),
                    mid_price: r.get("mid_price"),
                    trade_amount: r.get("trade_amount"),
                    buy_avg_price: r.get("buy_avg_price"),
                    buy_slippage_bps: r.get("buy_slippage_bps"),
                    buy_slippage_pct: r.get("buy_slippage_pct"),
                    buy_total_cost: None, // Removed from response
                    buy_feasible: r.get("buy_feasible"),
                    sell_avg_price: r.get("sell_avg_price"),
                    sell_slippage_bps: r.get("sell_slippage_bps"),
                    sell_slippage_pct: r.get("sell_slippage_pct"),
                    sell_total_cost: None, // Removed from response
                    sell_feasible: r.get("sell_feasible"),
                },
            }).collect()
        }
    };

    // Apply filters for exchange-specific queries (repository method doesn't support filters)
    if query.exchange.is_some() {
        slippages = filter_slippages_with_exchange(slippages, amount_filter);
    }

    Ok(Json(DataResponse {
        data: slippages,
        timestamp: Utc::now(),
    }))
}

/// Get slippage history within a time range
/// 
/// Returns historical slippage data for a symbol within the specified time range.
/// If exchange is specified: returns slippage for that specific exchange/symbol combination.
/// If exchange is omitted: returns slippage for that symbol across all exchanges.
/// Optional amount and side filters can be applied.
pub async fn get_slippage_history(
    State(state): State<AppState>,
    Query(query): Query<SlippageHistoryQuery>,
) -> Result<Json<PaginatedResponse<SlippageWithExchange>>, AppError> {
    // Validate query parameters
    query.validate()?;

    // Parse required amount parameter
    let amount_filter = match Decimal::from_str(query.amount().trim()) {
        Ok(amount) => amount,
        Err(_) => return Err(AppError::bad_request("Invalid amount parameter".to_string())),
    };

    let limit = query.validated_limit();
    let offset = query.validated_offset();
    let start = query.start_datetime();
    let end = query.end_datetime();

    // Get total count and data
    let (total_count, slippages): (i64, Vec<SlippageWithExchange>) = match &query.exchange {
        // Exchange provided: use existing repository method then filter
        Some(exchange) => {
            let slippage_data = state
                .repository
                .get_slippage(exchange, &query.symbol(), start, end, None) // Get all records first
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            // Convert to SlippageWithExchange
            let slippage_with_exchange: Vec<SlippageWithExchange> = slippage_data.into_iter()
                .map(|s| SlippageWithExchange {
                    exchange: exchange.clone(),
                    slippage: s,
                })
                .collect();

            // Apply filters
            let filtered_data = filter_slippages_with_exchange(slippage_with_exchange, amount_filter);
            let total = filtered_data.len() as i64;
            
            // Apply pagination manually
            let paginated_data = filtered_data
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect();

            (total, paginated_data)
        }
        // No exchange: get all exchanges for this symbol with filters
        None => {
            let (where_clause, filter_params) = build_sql_filters(amount_filter, 3); // 3 because $1=symbol, $2=start, $3=end
            
            // Get total count first
            let count_query = format!(r#"
                SELECT COUNT(*)
                FROM slippage s
                JOIN exchanges e ON s.exchange_id = e.id
                WHERE s.symbol = $1
                  AND s.ts >= $2 AND s.ts <= $3{}
                "#, where_clause);
            
            let symbol = query.symbol();
            let mut count_sql = sqlx::query_as(&count_query)
                .bind(&symbol)
                .bind(start)
                .bind(end);
            
            // Bind filter parameters to count query
            for param in &filter_params {
                count_sql = count_sql.bind(param);
            }
            
            let total: (i64,) = count_sql
                .fetch_one(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            // Calculate parameter indices for data query (limit and offset come after filter params)
            let limit_param_idx = 4 + filter_params.len(); // $1=symbol, $2=start, $3=end, then filter params, then limit
            let offset_param_idx = limit_param_idx + 1;
            
            // Get paginated data
            let data_query = format!(r#"
                SELECT s.symbol, s.mid_price, s.trade_amount,
                       s.buy_avg_price, s.buy_slippage_bps, s.buy_slippage_pct, s.buy_total_cost, s.buy_feasible,
                       s.sell_avg_price, s.sell_slippage_bps, s.sell_slippage_pct, s.sell_total_cost, s.sell_feasible,
                       s.ts, e.name as exchange_name
                FROM slippage s
                JOIN exchanges e ON s.exchange_id = e.id
                WHERE s.symbol = $1
                  AND s.ts >= $2 AND s.ts <= $3{}
                ORDER BY s.ts DESC
                LIMIT ${}
                OFFSET ${}
                "#, where_clause, limit_param_idx, offset_param_idx);

            let symbol = query.symbol();
            let mut data_sql = sqlx::query(&data_query)
                .bind(&symbol)
                .bind(start)
                .bind(end);
            
            // Bind filter parameters to data query
            for param in &filter_params {
                data_sql = data_sql.bind(param);
            }
            
            let rows = data_sql
                .bind(limit)
                .bind(offset)
                .fetch_all(&state.pool)
                .await
                .map_err(|e| AppError::database(e.to_string()))?;

            let slippage_data = rows.into_iter().map(|r| SlippageWithExchange {
                exchange: r.get("exchange_name"),
                slippage: Slippage {
                    symbol: r.get("symbol"),
                    timestamp: r.get("ts"),
                    mid_price: r.get("mid_price"),
                    trade_amount: r.get("trade_amount"),
                    buy_avg_price: r.get("buy_avg_price"),
                    buy_slippage_bps: r.get("buy_slippage_bps"),
                    buy_slippage_pct: r.get("buy_slippage_pct"),
                    buy_total_cost: None, // Removed from response
                    buy_feasible: r.get("buy_feasible"),
                    sell_avg_price: r.get("sell_avg_price"),
                    sell_slippage_bps: r.get("sell_slippage_bps"),
                    sell_slippage_pct: r.get("sell_slippage_pct"),
                    sell_total_cost: None, // Removed from response
                    sell_feasible: r.get("sell_feasible"),
                },
            }).collect();

            (total.0, slippage_data)
        }
    };

    Ok(Json(PaginatedResponse {
        pagination: PaginationMeta {
            total: Some(total_count),
            limit,
            offset,
            count: slippages.len(),
        },
        data: slippages,
    }))
}