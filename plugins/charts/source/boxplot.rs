//! Box plots: one box per category, or per value column.

use serde_json::json;

use crate::cols::{self, ColKind};
use crate::common::{self, Built, SeriesInfo, YRange, intro, label_or};
use crate::ctx::Ctx;
use crate::err::{Res, fail};
use crate::format::compact;
use crate::frame::{self, FrameSpec, Laid};
use crate::num::round_sig;
use crate::spec::{Legend, MAX_BOXES};
use crate::stats::{BoxStats, box_stats};
use crate::svg::{Style, Svg};
use crate::table::Table;

const MAX_OUTLIERS: usize = 100;

/// Draws the chart.
pub fn render(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    let spec = ctx.spec;
    // (label, values)
    let mut groups: Vec<(String, Vec<f64>)> = Vec::new();
    let value_name: String = match &spec.x {
        Some(xn) => {
            let xc = cols::column(t, xn)?;
            let yc = match spec.y.first() {
                Some(n) => cols::column(t, n)?,
                None => match (0..t.names.len())
                    .find(|c| *c != xc && cols::kind(t, *c) == ColKind::Numeric)
                {
                    Some(c) => c,
                    None => return fail("no column of numbers to summarise; name it in y"),
                },
            };
            if spec.y.len() > 1 {
                ctx.notes
                    .add("with x, a box plot uses the first column in y; the others were ignored");
            }
            let n = cols::numeric(t, yc);
            cols::report_numeric(&mut ctx.notes, &t.names[yc], &n);
            let labels = cols::labels(t, xc);
            let (names, idx) = cols::distinct(&labels);
            let mut per: Vec<Vec<f64>> = vec![Vec::new(); names.len()];
            for (row, g) in idx.iter().enumerate() {
                if let Some(g) = g
                    && !n.v[row].is_nan()
                {
                    per[*g].push(n.v[row]);
                }
            }
            let totals: Vec<f64> = per.iter().map(|v| v.iter().sum()).collect();
            let order = cols::order(&names, &totals, spec.sort);
            if order.len() > MAX_BOXES {
                ctx.notes.add(format!(
                    "there are {} groups; only the first {MAX_BOXES} are drawn",
                    order.len()
                ));
            }
            for i in order.into_iter().take(MAX_BOXES) {
                groups.push((names[i].clone(), std::mem::take(&mut per[i])));
            }
            t.names[yc].clone()
        }
        None => {
            let ycols: Vec<usize> = if spec.y.is_empty() {
                (0..t.names.len())
                    .filter(|c| cols::kind(t, *c) == ColKind::Numeric)
                    .collect()
            } else {
                spec.y
                    .iter()
                    .map(|n| cols::column(t, n))
                    .collect::<Res<_>>()?
            };
            if ycols.is_empty() {
                return fail("no column of numbers to summarise; name it in y");
            }
            if ycols.len() > MAX_BOXES {
                ctx.notes.add(format!(
                    "there are {} value columns; only the first {MAX_BOXES} are drawn",
                    ycols.len()
                ));
            }
            for c in ycols.into_iter().take(MAX_BOXES) {
                let n = cols::numeric(t, c);
                cols::report_numeric(&mut ctx.notes, &t.names[c], &n);
                groups.push((
                    t.names[c].clone(),
                    n.v.into_iter().filter(|v| !v.is_nan()).collect(),
                ));
            }
            String::new()
        }
    };
    let stats: Vec<Option<BoxStats>> = groups.iter().map(|g| box_stats(&g.1)).collect();
    if stats.iter().all(Option::is_none) {
        return fail("there are no numbers to summarise; check the value column");
    }
    let empty: Vec<&str> = groups
        .iter()
        .zip(&stats)
        .filter(|(_, s)| s.is_none())
        .map(|(g, _)| g.0.as_str())
        .collect();
    if !empty.is_empty() {
        ctx.notes.add(format!(
            "no numbers for {}; those boxes are left empty",
            empty
                .iter()
                .map(|e| crate::text::quote(e))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if spec.y_log {
        for g in &groups {
            common::check_log("y", &value_name, &g.1)?;
        }
    }
    let y_extent = common::range(stats.iter().flatten().flat_map(|s| [s.min, s.max]))
        .ok_or("there are no numbers to summarise")?;
    let labels: Vec<String> = groups.iter().map(|g| g.0.clone()).collect();
    let x_label = label_or(&spec.x_label, spec.x.as_deref().unwrap_or(""));
    let y_label = label_or(&spec.y_label, &value_name);
    let xf = |_t: usize| crate::axes::band(labels.clone(), x_label.clone());
    let yf = |target: usize| {
        common::y_axis(
            spec,
            YRange {
                extent: y_extent,
                zero: false,
                log: spec.y_log,
                pad: 0.05,
                integer: false,
            },
            target,
            spec.y_format.as_ref(),
            y_label.clone(),
        )
    };
    let Laid { frame, y, .. } = frame::layout(
        ctx,
        &FrameSpec {
            x: &xf,
            y: &yf,
            y2: None,
            legend: &[],
            extra_right: 0.0,
            auto_legend: Legend::None,
        },
    )?;

    let mut svg = Svg::new(ctx.w, ctx.h, ctx.theme.bg);
    frame::draw_title(&mut svg, ctx, &frame);
    frame::draw_grid(&mut svg, ctx, &frame);
    let color = ctx.color(0);
    let slot = match frame.x.scale {
        frame::AxisScale::Band(b) => b.step().abs(),
        frame::AxisScale::Cont(_) => 40.0,
    };
    let half = (slot * 0.5 / 2.0).clamp(4.0, 60.0);
    let ys = |v: f64| frame.y.scale.at(v);
    let mut info = Vec::new();
    let mut clipped = 0usize;
    for (i, (g, s)) in groups.iter().zip(&stats).enumerate() {
        let Some(s) = s else { continue };
        let cx = frame.x.scale.at(i as f64);
        let line = Style::stroke(&color, 1.5);
        svg.line(cx, ys(s.whisker_lo), cx, ys(s.q1), &line);
        svg.line(cx, ys(s.q3), cx, ys(s.whisker_hi), &line);
        svg.line(
            cx - half / 2.0,
            ys(s.whisker_lo),
            cx + half / 2.0,
            ys(s.whisker_lo),
            &line,
        );
        svg.line(
            cx - half / 2.0,
            ys(s.whisker_hi),
            cx + half / 2.0,
            ys(s.whisker_hi),
            &line,
        );
        let tip = format!(
            "{}: n {}, median {}, q1 {}, q3 {}, min {}, max {}",
            g.0,
            s.n,
            compact(s.median),
            compact(s.q1),
            compact(s.q3),
            compact(s.min),
            compact(s.max)
        );
        let (top, bottom) = (ys(s.q3), ys(s.q1));
        svg.rect_titled(
            cx - half,
            top.min(bottom),
            half * 2.0,
            (bottom - top).abs().max(1.0),
            &Style::fill(&color).fill_alpha(0.35).outlined(&color, 1.5),
            &tip,
        );
        svg.line(
            cx - half,
            ys(s.median),
            cx + half,
            ys(s.median),
            &Style::stroke(ctx.theme.fg, 2.0),
        );
        for o in s.outliers.iter().take(MAX_OUTLIERS) {
            svg.circle_titled(
                cx,
                ys(*o),
                3.0,
                &Style::fill("none").outlined(&color, 1.25),
                &format!("{}: outlier {}", g.0, compact(*o)),
            );
        }
        clipped += s.outliers.len().saturating_sub(MAX_OUTLIERS);
        let mut extra = serde_json::Map::new();
        for (k, v) in [
            ("q1", s.q1),
            ("median", s.median),
            ("q3", s.q3),
            ("whisker_low", s.whisker_lo),
            ("whisker_high", s.whisker_hi),
        ] {
            extra.insert(k.into(), json!(round_sig(v, 12)));
        }
        extra.insert("outlier_count".into(), json!(s.outliers.len()));
        extra.insert(
            "outliers".into(),
            json!(
                s.outliers
                    .iter()
                    .take(20)
                    .map(|o| round_sig(*o, 12))
                    .collect::<Vec<_>>()
            ),
        );
        info.push(SeriesInfo {
            name: g.0.clone(),
            mark: "box",
            color: color.clone(),
            axis: "left",
            points: s.n,
            min: Some(s.min),
            max: Some(s.max),
            extra,
            ..SeriesInfo::default()
        });
    }
    if clipped > 0 {
        ctx.notes.add(format!("{clipped} outliers beyond the first {MAX_OUTLIERS} per box are counted in the statistics but not drawn"));
    }
    frame::draw_axes(&mut svg, ctx, &frame);

    let mut alt = intro("Box plot", spec.title.as_deref());
    alt.push_str(&format!(" {} boxes", info.len()));
    if !value_name.is_empty() {
        alt.push_str(&format!(" of {value_name}"));
    }
    alt.push('.');
    if let Some((d0, d1)) = y.domain() {
        alt.push(' ');
        alt.push_str(&common::axis_sentence(
            "Y",
            y.label.as_deref(),
            &compact(d0),
            &compact(d1),
        ));
    }
    for (g, s) in groups.iter().zip(&stats).take(8) {
        if let Some(s) = s {
            alt.push_str(&format!(
                " {}: median {}, quartiles {} to {}, range {} to {}, {}.",
                crate::text::quote(&g.0),
                compact(s.median),
                compact(s.q1),
                compact(s.q3),
                compact(s.min),
                compact(s.max),
                crate::common::count_of(s.outliers.len(), "outlier")
            ));
        }
    }
    if groups.len() > 8 {
        alt.push_str(&format!(" And {} more boxes.", groups.len() - 8));
    }
    Ok(Built {
        svg,
        series: info,
        alt,
        plot: frame.plot,
    })
}
