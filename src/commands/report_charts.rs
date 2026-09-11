//! PNG chart rendering for `report --time-series`. Generic over any labeled
//! multi-series time-series data — the caller (report.rs) groups DB rows into
//! per-exchange series before calling `plot_metric_chart`. Kept decoupled from
//! `stats.rs`'s charting helpers (same underlying pattern, ported rather than
//! shared — see docs/plan/report_time_series_charts.md).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use plotters::coord::Shift;
use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};
use std::path::Path;

/// Deterministic per-exchange color, hash-fallback for names outside the
/// hardcoded list (ported from `stats.rs::get_color_for_label`).
fn get_color_for_label(label: &str) -> RGBColor {
    match label.to_lowercase().as_str() {
        "binance" => RGBColor(255, 127, 14),
        "extended" => RGBColor(214, 39, 40),
        "hyperliquid" => RGBColor(44, 160, 44),
        "nadex" => RGBColor(31, 119, 180),
        "pacifica" => RGBColor(23, 190, 207),
        "lighter" => RGBColor(148, 103, 189),
        "bybit" => RGBColor(227, 119, 194),
        "okx" => RGBColor(188, 189, 34),
        "deribit" => RGBColor(140, 86, 75),
        "btc" => RGBColor(247, 147, 26),
        "eth" => RGBColor(98, 126, 234),
        "sol" => RGBColor(220, 31, 255),
        "arb" => RGBColor(40, 160, 240),
        "avax" => RGBColor(232, 65, 66),
        "op" => RGBColor(255, 4, 32),
        _ => {
            let hash = label
                .bytes()
                .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
            let colors = [
                RGBColor(31, 119, 180),
                RGBColor(255, 127, 14),
                RGBColor(44, 160, 44),
                RGBColor(214, 39, 40),
                RGBColor(148, 103, 189),
                RGBColor(140, 86, 75),
                RGBColor(227, 119, 194),
                RGBColor(127, 127, 127),
                RGBColor(188, 189, 34),
                RGBColor(23, 190, 207),
            ];
            colors[(hash as usize) % colors.len()]
        }
    }
}

/// Min/Mean/Median/95th percentile/Max/StdDev over one series' plotted
/// (bucketed) values — NOT the whole-range raw-tick aggregate shown in the
/// report tables above the chart. Computed here (not via a fresh query) since
/// this table describes "the line you're looking at", not the underlying raw
/// data — callers should call this out near the embedded image.
struct SeriesStats {
    min: f64,
    mean: f64,
    median: f64,
    p95: f64,
    max: f64,
    stddev: f64,
}

fn compute_stats(values: &[f64]) -> SeriesStats {
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len() as f64;
    let mean = sorted.iter().sum::<f64>() / n;
    let variance = sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    SeriesStats {
        min: sorted[0],
        mean,
        median: percentile_cont(&sorted, 0.5),
        p95: percentile_cont(&sorted, 0.95),
        max: sorted[sorted.len() - 1],
        stddev: variance.sqrt(),
    }
}

/// Linear-interpolation percentile over an already-sorted, non-empty slice
/// (matches Postgres PERCENTILE_CONT, mirroring `report.rs::percentile_cont`).
fn percentile_cont(sorted: &[f64], p: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Formats a chart value compactly (k/M suffix) for axis labels and the legend
/// table's numeric columns.
fn fmt_stat(v: f64) -> String {
    let abs = v.abs();
    if abs >= 1_000_000.0 {
        format!("{:.2}M", v / 1_000_000.0)
    } else if abs >= 1_000.0 {
        format!("{:.2}k", v / 1_000.0)
    } else {
        format!("{:.3}", v)
    }
}

/// Renders one multi-exchange line chart to `output_path`. The legend is a
/// Grafana-style summary-stats table below the chart (not an inline legend
/// box): one row per series with a color swatch matching that series' line,
/// plus Min/Mean/Median/95th %/Max/StdDev computed from the plotted points.
///
/// # Errors
/// Returns an error if every series is empty, or if the PNG cannot be written.
pub fn plot_metric_chart(
    output_path: &Path,
    title: &str,
    y_label: &str,
    series: &[(String, Vec<(DateTime<Utc>, f64)>)],
) -> Result<()> {
    let non_empty: Vec<&(String, Vec<(DateTime<Utc>, f64)>)> =
        series.iter().filter(|(_, pts)| !pts.is_empty()).collect();
    if non_empty.is_empty() {
        anyhow::bail!("no data points to plot");
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create chart directory {}", parent.display()))?;
    }

    const WIDTH: u32 = 1200;
    const CHART_H: u32 = 800;
    const ROW_H: i32 = 28;

    let table_h = ROW_H as u32 * (non_empty.len() as u32 + 1) + 20;
    let root = BitMapBackend::new(output_path, (WIDTH, CHART_H + table_h)).into_drawing_area();
    root.fill(&WHITE)?;
    let (chart_area, table_area) = root.split_vertically(CHART_H);

    let min_ts = non_empty
        .iter()
        .flat_map(|(_, pts)| pts.iter().map(|(t, _)| *t))
        .min()
        .context("no timestamps")?;
    let max_ts = non_empty
        .iter()
        .flat_map(|(_, pts)| pts.iter().map(|(t, _)| *t))
        .max()
        .context("no timestamps")?;
    let all_vals: Vec<f64> = non_empty
        .iter()
        .flat_map(|(_, pts)| pts.iter().map(|(_, v)| *v))
        .collect();
    let min_v = all_vals.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_v = all_vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let (y_lo, y_hi) = if (max_v - min_v).abs() < f64::EPSILON {
        (min_v - 1.0, max_v + 1.0)
    } else {
        let pad = (max_v - min_v) * 0.1;
        (min_v - pad, max_v + pad)
    };

    let mut chart = ChartBuilder::on(&chart_area)
        .caption(title, ("sans-serif", 24).into_font())
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(80)
        .build_cartesian_2d(min_ts..max_ts, y_lo..y_hi)?;

    chart
        .configure_mesh()
        .x_desc("Time (UTC)")
        .y_desc(y_label)
        .x_label_formatter(&|x| x.format("%m-%d %Hh").to_string())
        .y_label_formatter(&|y| fmt_stat(*y))
        .draw()?;

    let mut rows: Vec<(String, RGBColor, SeriesStats)> = Vec::new();
    for (label, points) in &non_empty {
        let color = get_color_for_label(label);
        chart.draw_series(LineSeries::new(
            points.iter().map(|(t, v)| (*t, *v)),
            &color,
        ))?;
        let values: Vec<f64> = points.iter().map(|(_, v)| *v).collect();
        rows.push((label.clone(), color, compute_stats(&values)));
    }

    draw_legend_table(&table_area, &rows, ROW_H)?;

    root.present().context("Failed to write chart PNG")?;
    Ok(())
}

/// Draws the bottom legend table: header row + one row per series (color
/// swatch, name, Min/Mean/Median/95th %/Max/StdDev), at fixed pixel x-offsets.
fn draw_legend_table(
    area: &DrawingArea<BitMapBackend, Shift>,
    rows: &[(String, RGBColor, SeriesStats)],
    row_h: i32,
) -> Result<()> {
    const COL_X: [i32; 7] = [16, 220, 380, 540, 700, 860, 1020];
    const HEADERS: [&str; 7] = ["Name", "Min", "Mean", "Median", "95th %", "Max", "StdDev"];

    let header_style = TextStyle::from(("sans-serif", 14).into_font())
        .color(&BLACK)
        .pos(Pos::new(HPos::Left, VPos::Top));
    for (i, h) in HEADERS.iter().enumerate() {
        area.draw(&Text::new(*h, (COL_X[i], 6), &header_style))?;
    }

    for (r, (label, color, stats)) in rows.iter().enumerate() {
        let y = row_h * (r as i32 + 1) + 6;
        area.draw(&Rectangle::new(
            [(COL_X[0] - 12, y + 2), (COL_X[0] - 2, y + 14)],
            color.filled(),
        ))?;
        let name_style = TextStyle::from(("sans-serif", 13).into_font())
            .color(color)
            .pos(Pos::new(HPos::Left, VPos::Top));
        area.draw(&Text::new(label.as_str(), (COL_X[0], y), &name_style))?;

        let value_style = TextStyle::from(("sans-serif", 13).into_font())
            .color(&BLACK)
            .pos(Pos::new(HPos::Left, VPos::Top));
        let values = [
            stats.min,
            stats.mean,
            stats.median,
            stats.p95,
            stats.max,
            stats.stddev,
        ];
        for (i, v) in values.iter().enumerate() {
            area.draw(&Text::new(fmt_stat(*v), (COL_X[i + 1], y), &value_style))?;
        }
    }
    Ok(())
}
