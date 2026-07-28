use anyhow::Result;
use perps_exchanges::get_exchange;
use prettytable::{format, Cell, Row, Table};
use rust_decimal::Decimal;
use serde_json;

pub struct MarketsArgs {
    pub exchanges: String,
    pub symbols: Option<String>,
    pub format: String,
}

struct ExchangeMarket {
    exchange: String,
    market: perps_core::Market,
}

pub async fn execute(args: MarketsArgs) -> Result<()> {
    let exchange_names: Vec<&str> = args.exchanges.split(',').map(str::trim).collect();
    let sym_filter: Option<Vec<String>> = args.symbols.as_deref().map(|s| {
        s.split(',').map(|x| x.trim().to_uppercase()).collect()
    });

    let mut rows: Vec<ExchangeMarket> = Vec::new();

    for name in &exchange_names {
        let client = match get_exchange(name).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Skipping exchange {}: {}", name, e);
                continue;
            }
        };

        let mut markets = match client.get_markets().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Failed to get markets for {}: {}", name, e);
                continue;
            }
        };

        if let Some(filter) = &sym_filter {
            // parse_symbol normalises the global symbol to exchange format, then we match
            // against the already-normalised market.symbol
            markets.retain(|m| filter.contains(&m.symbol.to_uppercase()));
        }

        for m in markets {
            rows.push(ExchangeMarket {
                exchange: name.to_string(),
                market: m,
            });
        }
    }

    if rows.is_empty() {
        anyhow::bail!("No markets found");
    }

    rows.sort_by(|a, b| a.market.symbol.cmp(&b.market.symbol).then(a.exchange.cmp(&b.exchange)));

    match args.format.as_str() {
        "json" => display_json(&rows),
        "csv" => display_csv(&rows),
        _ => display_table(&rows),
    }
}

fn step_from_scale(scale: i32) -> Decimal {
    if scale >= 0 {
        Decimal::new(1, scale as u32)
    } else {
        // negative scale means multiples (e.g. scale=-2 → step=100)
        let exp = (-scale) as u32;
        Decimal::new(10_i64.pow(exp), 0)
    }
}

fn display_table(rows: &[ExchangeMarket]) -> Result<()> {
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
    table.set_titles(Row::new(vec![
        Cell::new("Exchange"),
        Cell::new("Symbol"),
        Cell::new("Contract"),
        Cell::new("StepPrice"),
        Cell::new("StepSize"),
        Cell::new("MinQty"),
        Cell::new("MaxQty"),
        Cell::new("MaxLeverage"),
    ]));

    for r in rows {
        let m = &r.market;
        let step_price = step_from_scale(m.price_scale);
        let step_size = step_from_scale(m.quantity_scale);
        table.add_row(Row::new(vec![
            Cell::new(&r.exchange),
            Cell::new(&m.symbol),
            Cell::new(&m.contract),
            Cell::new(&step_price.to_string()),
            Cell::new(&step_size.to_string()),
            Cell::new(&m.min_order_qty.to_string()),
            Cell::new(&m.max_order_qty.to_string()),
            Cell::new(&m.max_leverage.to_string()),
        ]));
    }

    table.printstd();
    Ok(())
}

fn display_json(rows: &[ExchangeMarket]) -> Result<()> {
    let values: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let m = &r.market;
            let step_price = step_from_scale(m.price_scale);
            let step_size = step_from_scale(m.quantity_scale);
            serde_json::json!({
                "exchange": r.exchange,
                "symbol": m.symbol,
                "contract": m.contract,
                "price_scale": m.price_scale,
                "step_price": step_price.to_string(),
                "quantity_scale": m.quantity_scale,
                "step_size": step_size.to_string(),
                "min_order_qty": m.min_order_qty.to_string(),
                "max_order_qty": m.max_order_qty.to_string(),
                "min_order_value": m.min_order_value.to_string(),
                "max_leverage": m.max_leverage.to_string(),
            })
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&values)?);
    Ok(())
}

fn display_csv(rows: &[ExchangeMarket]) -> Result<()> {
    let mut wtr = csv::Writer::from_writer(std::io::stdout());
    wtr.write_record([
        "exchange",
        "symbol",
        "contract",
        "price_scale",
        "step_price",
        "quantity_scale",
        "step_size",
        "min_order_qty",
        "max_order_qty",
        "min_order_value",
        "max_leverage",
    ])?;

    for r in rows {
        let m = &r.market;
        let step_price = step_from_scale(m.price_scale);
        let step_size = step_from_scale(m.quantity_scale);
        wtr.write_record(&[
            &r.exchange,
            &m.symbol,
            &m.contract,
            &m.price_scale.to_string(),
            &step_price.to_string(),
            &m.quantity_scale.to_string(),
            &step_size.to_string(),
            &m.min_order_qty.to_string(),
            &m.max_order_qty.to_string(),
            &m.min_order_value.to_string(),
            &m.max_leverage.to_string(),
        ])?;
    }
    wtr.flush()?;
    Ok(())
}
