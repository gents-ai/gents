//! Bar charts (grouped, stacked, horizontal) and combined bar and line
//! charts with a second axis.

use serde_json::json;

use crate::axes::{self, NumSpec};
use crate::cols::{self, ColKind};
use crate::common::{self, Built, SeriesInfo, YRange, intro, label_or};
use crate::ctx::Ctx;
use crate::err::{Res, fail};
use crate::format::compact;
use crate::frame::{self, Axis, AxisScale, FrameSpec, Laid, LegendItem, Swatch};
use crate::marks::{draw_lines, split_runs};
use crate::shape::{self, XPolicy};
use crate::spec::{Kind, Legend};
use crate::stats::stack;
use crate::svg::{Anchor, Style, Svg, TextStyle};
use crate::table::Table;

fn value_text(ctx: &Ctx<'_>, v: f64) -> String {
    match &ctx.spec.y_format {
        Some(f) => f.apply(v, 2),
        None => compact(v),
    }
}

fn extents(values: impl IntoIterator<Item = f64>) -> Option<(f64, f64)> {
    common::range(values)
}

/// Draws the chart.
pub fn render(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    let spec = ctx.spec;
    let combo = spec.kind == Kind::Combo;
    let horizontal = spec.horizontal && !combo;
    if combo {
        if spec.line.is_empty() {
            return fail("a combo chart needs line columns; name them in line");
        }
        if spec.series.is_some() {
            return fail("a combo chart takes value columns in y and line, not a series column");
        }
    }
    let line_cols: Vec<usize> = spec
        .line
        .iter()
        .map(|n| cols::column(t, n))
        .collect::<Res<_>>()?;
    let mut named = spec.y.clone();
    if combo {
        if named.is_empty() {
            let x_idx = spec
                .x
                .as_deref()
                .map(|n| cols::column(t, n))
                .transpose()?
                .unwrap_or(0);
            let defaults: Vec<usize> = (0..t.names.len())
                .filter(|c| {
                    *c != x_idx && !line_cols.contains(c) && cols::kind(t, *c) == ColKind::Numeric
                })
                .collect();
            if defaults.is_empty() {
                return fail("a combo chart needs bar columns; name them in y");
            }
            named = defaults.iter().map(|c| t.names[*c].clone()).collect();
        }
        named.extend(spec.line.iter().cloned());
    }
    let r = shape::resolve(ctx, t, XPolicy::Cat, &named, &[])?;
    let n_bars = if combo {
        r.groups.len() - line_cols.len()
    } else {
        r.groups.len()
    };
    let grid = r.grid(t, spec.agg);
    let ncat = r.x.cats.len();
    if ncat == 0 || grid.iter().all(|g| g.iter().all(|v| v.is_nan())) {
        return fail("there are no numbers to draw; check the value column");
    }
    if spec.y_log {
        for (g, row) in r.groups.iter().zip(&grid).take(n_bars) {
            common::check_log("y", &g.name, row)?;
        }
        if spec.stacked {
            return fail(
                "stacked bars need a linear y axis; use grouped bars for a logarithmic axis",
            );
        }
    }
    let stacked = spec.stacked && n_bars > 1;
    if spec.stacked && n_bars <= 1 {
        ctx.notes.add("stacked has no effect with one series");
    }
    let layers = if stacked {
        stack(&grid[..n_bars])
    } else {
        Vec::new()
    };
    let bar_values = grid[..n_bars].iter().flatten().copied();
    let bar_extent = if stacked {
        extents(
            layers
                .iter()
                .flat_map(|l| l.lo.iter().chain(&l.hi).copied()),
        )
    } else {
        extents(bar_values)
    };
    let line_grid = &grid[n_bars..];
    let line_extent = extents(line_grid.iter().flatten().copied());
    let lines_left = combo && !spec.line_right;
    let line_log = if lines_left { spec.y_log } else { spec.y2_log };
    if combo && line_log {
        let axis = if lines_left { "y" } else { "right y" };
        for (g, row) in r.groups[n_bars..].iter().zip(line_grid) {
            common::check_log(axis, &g.name, row)?;
        }
    }
    // Lines on the left axis share it with the bars, so it spans both.
    let bar_extent = if lines_left {
        extents(
            bar_extent
                .into_iter()
                .chain(line_extent)
                .flat_map(|(lo, hi)| [lo, hi]),
        )
    } else {
        bar_extent
    };
    let bar_extent = bar_extent.unwrap_or((0.0, 1.0));

    let legend: Vec<LegendItem> = r
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| LegendItem {
            label: g.name.clone(),
            color: ctx.color(i),
            swatch: if i < n_bars {
                Swatch::Box
            } else {
                Swatch::Line
            },
        })
        .collect();
    let single_bar_label = if n_bars == 1 {
        r.groups[0].name.clone()
    } else {
        String::new()
    };
    let value_label = label_or(&spec.y_label, &single_bar_label);
    let cat_label = label_or(&spec.x_label, &r.x.name);
    let cat_axis = |label: Option<String>| axes::band(r.x.cats.clone(), label);
    let val_axis = |target: usize, label: Option<String>| {
        common::y_axis(
            spec,
            YRange {
                extent: bar_extent,
                zero: true,
                log: spec.y_log,
                pad: 0.06,
                integer: spec.agg == crate::spec::Agg::Count,
            },
            target,
            spec.y_format.as_ref(),
            label,
        )
    };
    let xf = |target: usize| {
        if horizontal {
            val_axis(target, value_label.clone())
        } else {
            cat_axis(cat_label.clone())
        }
    };
    let yf = |target: usize| {
        if horizontal {
            cat_axis(cat_label.clone())
        } else {
            val_axis(target, value_label.clone())
        }
    };
    let y2_label = label_or(
        &spec.y2_label,
        &if line_cols.len() == 1 {
            r.groups[n_bars].name.clone()
        } else {
            String::new()
        },
    );
    let y2f = |target: usize| -> Axis {
        let (lo, hi) = line_extent.unwrap_or((0.0, 1.0));
        let (lo, hi) = if spec.y2_log {
            (lo, hi)
        } else {
            common::padded((lo, hi), 0.06, false)
        };
        axes::numeric(
            NumSpec {
                min: lo,
                max: hi,
                user_min: None,
                user_max: None,
                zero: false,
                nice: true,
                log: spec.y2_log,
                integer: false,
            },
            target,
            spec.y2_format.as_ref(),
            y2_label.clone(),
        )
    };
    let fs = FrameSpec {
        x: &xf,
        y: &yf,
        y2: if combo && !lines_left && line_extent.is_some() {
            Some(&y2f)
        } else {
            None
        },
        legend: &legend,
        extra_right: 0.0,
        auto_legend: Legend::Auto,
    };
    let Laid { frame, x, y, y2 } = frame::layout(ctx, &fs)?;

    let mut svg = Svg::new(ctx.w, ctx.h, ctx.theme.bg);
    frame::draw_title(&mut svg, ctx, &frame);
    frame::draw_grid(&mut svg, ctx, &frame);
    let (cat, val) = match (horizontal, &frame.x.scale, &frame.y.scale) {
        (false, AxisScale::Band(b), AxisScale::Cont(v))
        | (true, AxisScale::Cont(v), AxisScale::Band(b)) => (*b, *v),
        _ => return fail("the chart could not be laid out; check the data"),
    };
    for (k, (g, row)) in r.groups.iter().zip(&grid).take(n_bars).enumerate() {
        let tops: Vec<f64> = if stacked {
            layers[k].hi.clone()
        } else {
            row.clone()
        };
        common::note_cut(
            &mut ctx.notes,
            &g.name,
            "y",
            tops.into_iter(),
            spec.y_min,
            spec.y_max,
        );
    }
    let p = frame.plot;
    let clip =
        (spec.y_min.is_some() || spec.y_max.is_some()).then(|| svg.clip_rect(p.x, p.y, p.w, p.h));
    if let Some(c) = &clip {
        svg.open_group(c);
    }
    let (d_lo, d_hi) = (val.d0.min(val.d1), val.d0.max(val.d1));
    let base = val.map(if spec.y_log {
        d_lo
    } else {
        0.0f64.clamp(d_lo, d_hi)
    });
    let slot = cat.step().abs();
    let group_w = slot * 0.72;
    let bar_w = if stacked || n_bars == 1 {
        group_w
    } else {
        (group_w / n_bars as f64 - 1.5).max(1.0)
    };
    let label_bars = !stacked
        && ncat * n_bars <= 30
        && bar_w >= if horizontal { 0.0 } else { 22.0 }
        && (!horizontal || bar_w >= 12.0);
    let rect = |svg: &mut Svg, ci: usize, k: usize, lo: f64, hi: f64, color: &str, tip: &str| {
        let center = cat.center(ci);
        let off = if stacked || n_bars == 1 {
            -group_w / 2.0
        } else {
            -group_w / 2.0
                + k as f64 * (bar_w + 1.5)
                + (group_w - n_bars as f64 * (bar_w + 1.5) + 1.5) / 2.0
        };
        let (a, b) = (val.map(lo), val.map(hi));
        let (v0, v1) = (a.min(b), a.max(b));
        let thick = (v1 - v0).max(if (hi - lo).abs() > 0.0 { 1.0 } else { 0.0 });
        let v0 = if v1 - v0 < thick { v1 - thick } else { v0 };
        let st = Style::fill(color);
        if horizontal {
            let top = center + off;
            svg.rect_titled(v1 - thick, top, thick, bar_w, &st, tip);
        } else {
            svg.rect_titled(center + off, v0, bar_w, thick, &st, tip);
        }
    };
    let mut info = Vec::new();
    for (k, (g, row)) in r.groups.iter().zip(&grid).take(n_bars).enumerate() {
        let color = ctx.color(k);
        for (ci, v) in row.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            let (lo, hi) = if stacked {
                (layers[k].lo[ci], layers[k].hi[ci])
            } else {
                (
                    if spec.y_log {
                        d_lo
                    } else {
                        0.0f64.clamp(d_lo, d_hi)
                    },
                    *v,
                )
            };
            let tip = format!("{}, {}: {}", g.name, r.x.cats[ci], value_text(ctx, *v));
            rect(&mut svg, ci, k, lo, hi, &color, &tip);
            if label_bars {
                let txt = value_text(ctx, *v);
                let center = cat.center(ci);
                let off = if n_bars == 1 {
                    0.0
                } else {
                    -group_w / 2.0
                        + k as f64 * (bar_w + 1.5)
                        + (group_w - n_bars as f64 * (bar_w + 1.5) + 1.5) / 2.0
                        + bar_w / 2.0
                };
                let end = val.map(*v);
                let st = TextStyle::new(10.0, ctx.theme.fg);
                if horizontal {
                    let neg = *v < 0.0;
                    let st = if neg { st.anchor(Anchor::End) } else { st };
                    svg.text(
                        if neg { end - 4.0 } else { end + 4.0 },
                        center + off + 3.5,
                        &txt,
                        &st,
                    );
                } else {
                    let y_text = if *v < 0.0 { end + 12.0 } else { end - 4.0 };
                    svg.text(center + off, y_text, &txt, &st.anchor(Anchor::Middle));
                }
            }
        }
        let finite: Vec<f64> = row.iter().copied().filter(|v| !v.is_nan()).collect();
        let (min, max) = common::range(finite.iter().copied()).unzip();
        let mut extra = serde_json::Map::new();
        extra.insert(
            "sum".into(),
            json!(crate::num::round_sig(finite.iter().sum::<f64>(), 12)),
        );
        info.push(SeriesInfo {
            name: g.name.clone(),
            mark: "bar",
            color,
            axis: "left",
            points: finite.len(),
            gaps: row.len() - finite.len(),
            min,
            max,
            extra,
            ..SeriesInfo::default()
        });
    }
    if clip.is_some() {
        svg.close_group();
    }
    // Lines of a combo chart use the right axis unless asked otherwise.
    if combo {
        let ax = if spec.line_right {
            frame.y2.as_ref().map(|a| a.scale)
        } else {
            Some(frame.y.scale)
        };
        if let Some(AxisScale::Cont(line_scale)) = ax {
            for (k, (g, row)) in r.groups[n_bars..].iter().zip(line_grid).enumerate() {
                let color = ctx.color(n_bars + k);
                let px: Vec<(f64, f64)> = row
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        (
                            cat.center(i),
                            if v.is_nan() {
                                f64::NAN
                            } else {
                                line_scale.map(*v)
                            },
                        )
                    })
                    .collect();
                let keep: Vec<usize> = (0..px.len()).collect();
                draw_lines(&mut svg, &split_runs(&px, &keep), &color, 2.5);
                for (i, q) in px.iter().enumerate() {
                    if !q.1.is_nan() {
                        let tip = format!("{}, {}: {}", g.name, r.x.cats[i], compact(row[i]));
                        svg.circle_titled(
                            q.0,
                            q.1,
                            3.5,
                            &Style::fill(&color).outlined(ctx.theme.bg, 1.0),
                            &tip,
                        );
                    }
                }
                let finite: Vec<f64> = row.iter().copied().filter(|v| !v.is_nan()).collect();
                let (min, max) = common::range(finite.iter().copied()).unzip();
                info.push(SeriesInfo {
                    name: g.name.clone(),
                    mark: "line",
                    color,
                    axis: if spec.line_right { "right" } else { "left" },
                    points: finite.len(),
                    gaps: row.len() - finite.len(),
                    min,
                    max,
                    ..SeriesInfo::default()
                });
            }
        }
    }
    // The zero line over the bars, unless it is the axis line itself.
    let zero_line = Style::stroke(ctx.theme.axis, 1.0);
    if horizontal && (base - p.x).abs() > 1.0 {
        svg.line(
            base.round() + 0.5,
            p.y,
            base.round() + 0.5,
            p.bottom(),
            &zero_line,
        );
    } else if !horizontal && (base - p.bottom()).abs() > 1.0 {
        svg.line(
            p.x,
            base.round() + 0.5,
            p.right(),
            base.round() + 0.5,
            &zero_line,
        );
    }
    frame::draw_axes(&mut svg, ctx, &frame);
    frame::draw_legend(&mut svg, ctx, &frame, &legend);

    // Description.
    let kind_name = match (combo, horizontal, stacked) {
        (true, _, _) => "Combined bar and line chart",
        (false, true, true) => "Stacked horizontal bar chart",
        (false, true, false) => "Horizontal bar chart",
        (false, false, true) => "Stacked bar chart",
        (false, false, false) => "Bar chart",
    };
    let mut alt = intro(kind_name, spec.title.as_deref());
    let first = r.x.cats.first().cloned().unwrap_or_default();
    let last = r.x.cats.last().cloned().unwrap_or_default();
    alt.push_str(&format!(" {} categories", ncat));
    if let Some(l) = &cat_label {
        alt.push_str(&format!(" of {l}"));
    }
    alt.push_str(&format!(", from {first} to {last}."));
    if let Some((d0, d1)) = y.domain().or_else(|| x.domain()) {
        alt.push(' ');
        alt.push_str(&common::axis_sentence(
            "Value",
            value_label.as_deref(),
            &compact(d0),
            &compact(d1),
        ));
    }
    if combo && let Some((d0, d1)) = y2.as_ref().and_then(Axis::domain) {
        alt.push(' ');
        alt.push_str(&common::axis_sentence(
            "Right",
            y2_label.as_deref(),
            &compact(d0),
            &compact(d1),
        ));
    }
    alt.push_str(&format!(" {} series.", info.len()));
    for (k, s) in info.iter().enumerate().take(8) {
        let row = &grid[k];
        let pts: Vec<(f64, f64)> = row
            .iter()
            .enumerate()
            .map(|(i, v)| (i as f64, *v))
            .collect();
        match common::extremes(&pts) {
            Some((lo, hi)) => alt.push_str(&format!(
                " {} ({}): highest {} at {}, lowest {} at {}.",
                crate::text::quote(&s.name),
                s.mark,
                compact(hi.1),
                r.x.cats[hi.0 as usize],
                compact(lo.1),
                r.x.cats[lo.0 as usize]
            )),
            None => alt.push_str(&format!(" {}: no numbers.", crate::text::quote(&s.name))),
        }
    }
    if info.len() > 8 {
        alt.push_str(&format!(" And {} more series.", info.len() - 8));
    }
    Ok(Built {
        svg,
        series: info,
        alt,
        plot: frame.plot,
    })
}
