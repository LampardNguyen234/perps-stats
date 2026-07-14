use anyhow::Result;
use perps_core::MultiResolutionOrderbook;
use perps_exchanges::get_exchange;
use prettytable::{format, Cell, Row, Table};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::HashMap;

pub struct LiqDistArgs {
    pub exchange: String,
    pub symbols: String,
    pub bps: String,
    /// "bid", "ask", "combined", or "all"
    pub side: String,
    pub format: String,
}

#[derive(Clone)]
struct SymbolStats {
    bid: Decimal,
    ask: Decimal,
    /// true if orderbook bid side is shallower than the requested bps level
    bid_partial: bool,
    /// true if orderbook ask side is shallower than the requested bps level
    ask_partial: bool,
}

#[derive(Serialize)]
struct DistEntry {
    bps: f64,
    exchange: String,
    symbol: String,
    bid_notional: f64,
    ask_notional: f64,
    combined_notional: f64,
    bid_pct: f64,
    ask_pct: f64,
    combined_pct: f64,
    bid_partial: bool,
    ask_partial: bool,
}

fn format_notional(d: Decimal, partial: bool) -> String {
    let v: f64 = d.to_string().parse().unwrap_or(0.0);
    let s = if v >= 1_000_000.0 {
        format!("${:.2}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("${:.1}K", v / 1_000.0)
    } else {
        format!("${:.2}", v)
    };
    if partial { format!("{}*", s) } else { s }
}

fn parse_bps(bps_str: &str) -> Result<Vec<f64>> {
    bps_str
        .split(',')
        .map(|s| {
            let v = s
                .trim()
                .parse::<f64>()
                .map_err(|_| anyhow::anyhow!("Invalid bps value: '{}'", s.trim()))?;
            if v <= 0.0 {
                anyhow::bail!("bps must be positive, got {}", v);
            }
            Ok(v)
        })
        .collect()
}

fn bps_dec(bps: f64) -> Decimal {
    bps.to_string().parse().unwrap_or(Decimal::ZERO)
}

pub async fn execute(args: LiqDistArgs) -> Result<()> {
    let exchange_names: Vec<String> = args
        .exchange
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let symbols: Vec<String> = args
        .symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let bps_levels = parse_bps(&args.bps)?;

    let sides = sides_to_print(&args.side);

    // Fetch raw orderbooks: exchange → symbol → MultiResolutionOrderbook
    let mut data: HashMap<String, HashMap<String, MultiResolutionOrderbook>> = HashMap::new();
    for exchange_name in &exchange_names {
        let client = get_exchange(exchange_name).await?;
        let mut sym_data = HashMap::new();
        for symbol in &symbols {
            let parsed = client.parse_symbol(symbol);
            match client.get_orderbook(&parsed, 1000).await {
                Ok(multi_ob) => {
                    sym_data.insert(symbol.clone(), multi_ob);
                }
                Err(e) => tracing::error!(
                    "Failed orderbook for {}/{}: {}",
                    exchange_name,
                    symbol,
                    e
                ),
            }
        }
        data.insert(exchange_name.clone(), sym_data);
    }

    match args.format.as_str() {
        "json" => {
            let mut entries: Vec<DistEntry> = Vec::new();
            for &bps in &bps_levels {
                for exchange in &exchange_names {
                    let stats = compute_stats(&data, exchange, &symbols, bps);
                    let bid_total: Decimal =
                        symbols.iter().filter_map(|s| stats.get(s)).map(|s| s.bid).sum();
                    let ask_total: Decimal =
                        symbols.iter().filter_map(|s| stats.get(s)).map(|s| s.ask).sum();
                    let comb_total = bid_total + ask_total;

                    for symbol in &symbols {
                        let s = match stats.get(symbol) {
                            Some(s) => s,
                            None => continue,
                        };
                        let comb = s.bid + s.ask;
                        entries.push(DistEntry {
                            bps,
                            exchange: exchange.clone(),
                            symbol: symbol.clone(),
                            bid_notional: s.bid.to_string().parse().unwrap_or(0.0),
                            ask_notional: s.ask.to_string().parse().unwrap_or(0.0),
                            combined_notional: comb.to_string().parse().unwrap_or(0.0),
                            bid_pct: pct(s.bid, bid_total),
                            ask_pct: pct(s.ask, ask_total),
                            combined_pct: pct(comb, comb_total),
                            bid_partial: s.bid_partial,
                            ask_partial: s.ask_partial,
                        });
                    }
                }
            }
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
        _ => {
            if exchange_names.len() == 1 {
                // Single exchange: bps levels as rows, symbols as columns.
                let exchange = &exchange_names[0];
                let per_bps: Vec<(f64, HashMap<String, SymbolStats>)> = bps_levels
                    .iter()
                    .map(|&bps| (bps, compute_stats(&data, exchange, &symbols, bps)))
                    .collect();

                for side in &sides {
                    display_bps_rows_table(exchange, &per_bps, &symbols, side)?;
                }
            } else {
                // Compute once per bps level; reuse for both side tables and distribution.
                let all_bps_stats: Vec<(f64, HashMap<String, HashMap<String, SymbolStats>>)> =
                    bps_levels
                        .iter()
                        .map(|&bps| {
                            let stats = exchange_names
                                .iter()
                                .map(|e| (e.clone(), compute_stats(&data, e, &symbols, bps)))
                                .collect();
                            (bps, stats)
                        })
                        .collect();

                // Side tables: exchanges as rows, symbols as columns.
                for (bps, all_stats) in &all_bps_stats {
                    let label = bps_label(*bps);
                    for side in &sides {
                        let title = format!("{} Liquidity @ {} bps", side.label(), label);
                        display_side_table(&title, all_stats, &exchange_names, &symbols, side)?;
                    }
                }

                // Per-exchange tables at the end: bps as rows, symbols as columns.
                for exchange in &exchange_names {
                    let per_bps: Vec<(f64, HashMap<String, SymbolStats>)> = all_bps_stats
                        .iter()
                        .map(|(bps, all_stats)| {
                            (*bps, all_stats.get(exchange.as_str()).cloned().unwrap_or_default())
                        })
                        .collect();
                    for side in &sides {
                        display_bps_rows_table(exchange, &per_bps, &symbols, side)?;
                    }
                }
            }
        }
    }

    Ok(())
}

enum Side {
    Bid,
    Ask,
    Combined,
}

impl Side {
    fn label(&self) -> &'static str {
        match self {
            Side::Bid => "Bid",
            Side::Ask => "Ask",
            Side::Combined => "Combined",
        }
    }
}

fn sides_to_print(side: &str) -> Vec<Side> {
    match side {
        "bid" => vec![Side::Bid],
        "ask" => vec![Side::Ask],
        "all" => vec![Side::Bid, Side::Ask, Side::Combined],
        _ => vec![Side::Combined],
    }
}

fn pct(n: Decimal, total: Decimal) -> f64 {
    if total.is_zero() {
        return 0.0;
    }
    (n / total * Decimal::from(100))
        .to_string()
        .parse()
        .unwrap_or(0.0)
}

fn bps_label(bps: f64) -> String {
    if bps.fract() == 0.0 {
        format!("{}", bps as u64)
    } else {
        format!("{}", bps)
    }
}

/// Compute per-symbol bid/ask notionals and partial flags for one exchange.
fn compute_stats(
    data: &HashMap<String, HashMap<String, MultiResolutionOrderbook>>,
    exchange: &str,
    symbols: &[String],
    bps: f64,
) -> HashMap<String, SymbolStats> {
    let bps_d = bps_dec(bps);
    let sym_data = match data.get(exchange) {
        Some(d) => d,
        None => return HashMap::new(),
    };
    let mut out = HashMap::new();
    for symbol in symbols {
        if let Some(book) = sym_data.get(symbol) {
            let bid = book.bid_notional(bps_d);
            let ask = book.ask_notional(bps_d);
            let bid_partial = book
                .max_bid_bps()
                .map(|max| max < bps_d)
                .unwrap_or(true);
            let ask_partial = book
                .max_ask_bps()
                .map(|max| max < bps_d)
                .unwrap_or(true);
            out.insert(symbol.clone(), SymbolStats { bid, ask, bid_partial, ask_partial });
        }
    }
    out
}

/// Single-exchange view: rows = bps levels, columns = symbols.
fn display_bps_rows_table(
    exchange: &str,
    per_bps: &[(f64, HashMap<String, SymbolStats>)],
    symbols: &[String],
    side: &Side,
) -> Result<()> {
    let side_label = match side {
        Side::Bid => "Bid",
        Side::Ask => "Ask",
        Side::Combined => "Combined",
    };
    let title = format!("{} Liquidity — {}", side_label, exchange.to_uppercase());

    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);

    table.set_titles(Row::new(vec![Cell::new(&title)
        .with_hspan(symbols.len() + 1)
        .with_style(prettytable::Attr::Bold)
        .style_spec("c")]));

    let mut header = vec![Cell::new("BPS").with_style(prettytable::Attr::Bold)];
    for sym in symbols {
        header.push(Cell::new(sym).with_style(prettytable::Attr::Bold));
    }
    table.add_row(Row::new(header));

    let mut any_partial = false;

    for (bps, stats) in per_bps {
        let total: Decimal = symbols
            .iter()
            .filter_map(|s| stats.get(s))
            .map(|s| match side {
                Side::Bid => s.bid,
                Side::Ask => s.ask,
                Side::Combined => s.bid + s.ask,
            })
            .sum();

        let mut row = vec![Cell::new(&bps_label(*bps))];
        for sym in symbols {
            let cell = match stats.get(sym) {
                Some(s) => {
                    let (n, partial) = match side {
                        Side::Bid => (s.bid, s.bid_partial),
                        Side::Ask => (s.ask, s.ask_partial),
                        Side::Combined => (s.bid + s.ask, s.bid_partial || s.ask_partial),
                    };
                    if partial {
                        any_partial = true;
                    }
                    let p = pct(n, total);
                    Cell::new_align(
                        &format!("{} ({:.1}%)", format_notional(n, partial), p),
                        format::Alignment::RIGHT,
                    )
                }
                None => Cell::new_align("N/A", format::Alignment::RIGHT),
            };
            row.push(cell);
        }
        table.add_row(Row::new(row));
    }

    println!();
    table.printstd();
    if any_partial {
        println!("  * orderbook shallower than requested bps — value is a lower bound");
    }
    println!();

    Ok(())
}

/// Each exchange's % share of the cross-exchange total per symbol.
fn display_side_table(
    title: &str,
    all_stats: &HashMap<String, HashMap<String, SymbolStats>>,
    exchanges: &[String],
    symbols: &[String],
    side: &Side,
) -> Result<()> {
    // Compute totals per exchange for this side (for pct calculation)
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_LINESEP_WITH_TITLE);

    table.set_titles(Row::new(vec![Cell::new(title)
        .with_hspan(symbols.len() + 1)
        .with_style(prettytable::Attr::Bold)
        .style_spec("c")]));

    let mut header = vec![Cell::new("Venue").with_style(prettytable::Attr::Bold)];
    for sym in symbols {
        header.push(Cell::new(sym).with_style(prettytable::Attr::Bold));
    }
    table.add_row(Row::new(header));

    for exchange in exchanges {
        let stats = match all_stats.get(exchange) {
            Some(s) => s,
            None => continue,
        };

        let total: Decimal = symbols
            .iter()
            .filter_map(|s| stats.get(s))
            .map(|s| match side {
                Side::Bid => s.bid,
                Side::Ask => s.ask,
                Side::Combined => s.bid + s.ask,
            })
            .sum();

        let mut row = vec![Cell::new(&exchange.to_uppercase())];
        for sym in symbols {
            let cell = match stats.get(sym) {
                Some(s) => {
                    let (n, partial) = match side {
                        Side::Bid => (s.bid, s.bid_partial),
                        Side::Ask => (s.ask, s.ask_partial),
                        Side::Combined => (s.bid + s.ask, s.bid_partial || s.ask_partial),
                    };
                    let p = pct(n, total);
                    Cell::new_align(
                        &format!("{} ({:.1}%)", format_notional(n, partial), p),
                        format::Alignment::RIGHT,
                    )
                }
                None => Cell::new_align("N/A", format::Alignment::RIGHT),
            };
            row.push(cell);
        }
        table.add_row(Row::new(row));
    }

    // footnote only if any cell was partial
    let any_partial = exchanges.iter().any(|e| {
        all_stats.get(e).map(|stats| {
            symbols.iter().any(|s| {
                stats.get(s).map(|st| match side {
                    Side::Bid => st.bid_partial,
                    Side::Ask => st.ask_partial,
                    Side::Combined => st.bid_partial || st.ask_partial,
                }).unwrap_or(false)
            })
        }).unwrap_or(false)
    });

    println!();
    table.printstd();
    if any_partial {
        println!("  * orderbook shallower than requested bps — value is a lower bound");
    }
    println!();

    Ok(())
}
