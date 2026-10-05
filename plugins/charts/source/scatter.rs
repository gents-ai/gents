//! Scatter and bubble charts.

use serde_json::json;

use crate::cols;
use crate::common::{self, intro, label_or, Built, SeriesInfo};
use crate::ctx::Ctx;
use crate::err::{fail, Res};
use crate::format::compact;
use crate::frame::{self, FrameSpec, Laid, LegendItem, Swatch};
use crate::num::round_to;
use crate::reduce::bin_scatter;
use crate::shape::{self, XKind, XPolicy};
use crate::spec::{Kind, Legend, ScaleKind, MAX_SCATTER_POINTS};
use crate::svg::{Style, Svg};
use crate::table::Table;

/// Bubbles drawn at most.
const MAX_BUBBLES: usize = 2000;

/// Pearson correlation of the points, `None` below three points or without spread.
pub fn correlation(pts: &[(f64, f64)]) -> Option<f64> {
    let n = pts.len();
    if n < 3 {
        return None;
    }
    let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n as f64, pts.iter().map(|p| p.1).sum::<f64>() / n as f64);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for p in pts {
        let (dx, dy) = (p.0 - mx, p.1 - my);
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    let d = (sxx * syy).sqrt();
    (d > 0.0 && d.is_finite()).then(|| (sxy / d).clamp(-1.0, 1.0))
}

/// Draws the chart.
pub fn render(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    let spec = ctx.spec;
    let bubble = spec.kind == Kind::Bubble;
    let size_col = match (&spec.size, bubble) {
        (Some(n), _) => Some(cols::column(t, n)?),
        (None, true) => return fail("a bubble chart needs a size column; name it in size"),
        (None, false) => None,
    };
    if !bubble && spec.size.is_some() {
        ctx.notes.add("size only applies to bubble charts and was ignored");
    }
    let skip: Vec<usize> = size_col.into_iter().collect();
    let r = shape::resolve(ctx, t, XPolicy::Cont, &spec.y, &skip)?;
    let sizes = size_col.map(|c| cols::numeric(t, c));
    if let (Some(n), Some(c)) = (&sizes, size_col) {
        cols::report_numeric(&mut ctx.notes, &t.names[c], n);
    }
    // Points per series: (x, y, size).
    let mut series: Vec<(String, Vec<(f64, f64)>, Vec<f64>)> = Vec::new();
    for g in &r.groups {
        let y = r.numbers(g.col);
        let mut pts = Vec::new();
        let mut sz = Vec::new();
        let mut skipped = 0usize;
        for row in r.rows(g, t.rows) {
            let (xv, yv) = (r.x.v[row], y[row]);
            if xv.is_nan() || yv.is_nan() {
                continue;
            }
            if let Some(n) = &sizes {
                if n.v[row].is_nan() {
                    skipped += 1;
                    continue;
                }
                sz.push(n.v[row]);
            }
            pts.push((xv, yv));
        }
        if skipped > 0 {
            ctx.notes.add(format!("series {:?}: {skipped} points have no size and are not drawn", g.name));
        }
        series.push((g.name.clone(), pts, sz));
    }
    if series.iter().all(|s| s.1.is_empty()) {
        return fail("there are no points to draw; check the x and y columns");
    }
    if spec.y_log {
        for (g, s) in r.groups.iter().zip(&series) {
            common::check_log("y", &g.name, &s.1.iter().map(|p| p.1).collect::<Vec<_>>())?;
        }
    }
    let log_x = r.x.kind == XKind::Num && spec.x_scale == ScaleKind::Log;
    if log_x {
        let xs: Vec<f64> = series.iter().flat_map(|s| s.1.iter().map(|p| p.0)).collect();
        common::check_log("x", &r.x.name, &xs)?;
    }
    if bubble {
        for s in &mut series {
            if s.1.len() > MAX_BUBBLES {
                let mut order: Vec<usize> = (0..s.1.len()).collect();
                order.sort_by(|a, b| s.2[*b].total_cmp(&s.2[*a]).then(a.cmp(b)));
                order.truncate(MAX_BUBBLES);
                order.sort_unstable();
                ctx.notes.add(format!("series {:?} has {} bubbles; the {MAX_BUBBLES} largest are drawn", s.0, s.1.len()));
                s.1 = order.iter().map(|i| s.1[*i]).collect();
                s.2 = order.iter().map(|i| s.2[*i]).collect();
            }
        }
    }
    let x_extent = common::range(series.iter().flat_map(|s| s.1.iter().map(|p| p.0))).ok_or("there are no points to draw")?;
    let y_extent = common::range(series.iter().flat_map(|s| s.1.iter().map(|p| p.1))).ok_or("there are no numbers to draw")?;
    let size_extent = common::range(series.iter().flat_map(|s| s.2.iter().copied()));
    let x_pad = if log_x { x_extent } else { common::padded(x_extent, 0.04, false) };

    let legend: Vec<LegendItem> = r.groups.iter().enumerate().map(|(i, g)| LegendItem { label: g.name.clone(), color: ctx.color(i), swatch: Swatch::Dot }).collect();
    let y_label = label_or(&spec.y_label, &r.y_name);
    let xf = |target: usize| {
        let mut ex = x_pad;
        if r.x.kind == XKind::Time {
            ex = x_extent;
        }
        common::x_axis(spec, &r.x, ex, target, r.x.kind == XKind::Num)
    };
    let yf = |target: usize| common::y_axis(spec, y_extent, false, spec.y_log, 0.05, target, spec.y_format.as_ref(), y_label.clone());
    let Laid { frame, x, y, .. } = frame::layout(ctx, &FrameSpec { x: &xf, y: &yf, y2: None, legend: &legend, extra_right: 0.0, auto_legend: Legend::Auto })?;

    let mut svg = Svg::new(ctx.w, ctx.h, ctx.theme.bg);
    frame::draw_title(&mut svg, ctx, &frame);
    frame::draw_grid(&mut svg, ctx, &frame);
    let p = frame.plot;
    let clip = (spec.y_min.is_some() || spec.y_max.is_some() || spec.x_min.is_some() || spec.x_max.is_some()).then(|| svg.clip_rect(p.x, p.y, p.w, p.h));
    if let Some(c) = &clip {
        svg.open_group(c);
    }
    let total: usize = series.iter().map(|s| s.1.len()).sum();
    let per_series_cap = (MAX_SCATTER_POINTS / series.len().max(1)).max(500);
    let radius = if total > 1000 { 2.5 } else { 4.0 };
    let alpha = if total > 1000 { 0.5 } else { 0.78 };
    let max_r = (p.w.min(p.h) / 9.0).clamp(10.0, 28.0);
    let mut info = Vec::new();
    for (i, (name, pts, sz)) in series.iter().enumerate() {
        let color = ctx.color(i);
        let px: Vec<(f64, f64)> = pts.iter().map(|q| (frame.x.scale.at(q.0), frame.y.scale.at(q.1))).collect();
        let mut drawn = pts.len();
        if bubble {
            let (smin, smax) = size_extent.unwrap_or((0.0, 1.0));
            let mut order: Vec<usize> = (0..px.len()).collect();
            order.sort_by(|a, b| sz[*b].total_cmp(&sz[*a]).then(a.cmp(b)));
            for k in order {
                let norm = if smax > smin { (sz[k] - smin) / (smax - smin) } else { 0.5 };
                let rad = 4.0 + norm.sqrt() * (max_r - 4.0);
                let tip = format!("{name}: {}, {}, size {}", common::x_text(&r.x, pts[k].0), compact(pts[k].1), compact(sz[k]));
                svg.circle_titled(px[k].0, px[k].1, rad, &Style::fill(&color).fill_alpha(0.55).outlined(&color, 1.5), &tip);
            }
        } else if px.len() > per_series_cap {
            let binned = bin_scatter(&px, per_series_cap);
            drawn = binned.bins.len();
            ctx.notes.add(format!(
                "series {name:?} has {} points; they are drawn as {} grouped marks (cells of {} px, darker marks hold more points) and the extreme points stay exact",
                px.len(),
                binned.bins.len(),
                binned.cell
            ));
            let cells = binned.bins.len() - binned.extremes;
            for (k, b) in binned.bins.iter().enumerate() {
                let a = if k >= cells { 0.9 } else { (0.3 + 0.7 * (b.n.min(16) as f64 / 16.0)).min(0.95) };
                svg.circle(b.x, b.y, radius, &Style::fill(&color).fill_alpha(a));
            }
        } else {
            let tips = px.len() <= 200;
            for (k, q) in px.iter().enumerate() {
                let st = Style::fill(&color).fill_alpha(alpha).outlined(ctx.theme.bg, 0.75);
                if tips {
                    let tip = format!("{name}: {}, {}", common::x_text(&r.x, pts[k].0), compact(pts[k].1));
                    svg.circle_titled(q.0, q.1, radius, &st, &tip);
                } else {
                    svg.circle(q.0, q.1, radius, &st);
                }
            }
        }
        let (min, max) = common::range(pts.iter().map(|q| q.1)).unzip();
        let mut extra = serde_json::Map::new();
        if let Some((a, b)) = common::range(pts.iter().map(|q| q.0)) {
            extra.insert("x_min".into(), json!(round_to(a, 9)));
            extra.insert("x_max".into(), json!(round_to(b, 9)));
        }
        if let Some(c) = correlation(pts) {
            extra.insert("correlation".into(), json!(round_to(c, 4)));
        }
        info.push(SeriesInfo { name: name.clone(), mark: if bubble { "bubble" } else { "point" }, color, axis: "left", points: pts.len(), drawn, gaps: 0, min, max, extra });
    }
    if clip.is_some() {
        svg.close_group();
    }
    frame::draw_axes(&mut svg, ctx, &frame);
    frame::draw_legend(&mut svg, ctx, &frame, &legend);

    let mut alt = intro(if bubble { "Bubble chart" } else { "Scatter chart" }, spec.title.as_deref());
    alt.push(' ');
    alt.push_str(&common::axis_sentence("X", x.label.as_deref(), &common::x_text(&r.x, x_extent.0), &common::x_text(&r.x, x_extent.1)));
    if let Some((d0, d1)) = y.domain() {
        alt.push(' ');
        alt.push_str(&common::axis_sentence("Y", y.label.as_deref(), &compact(d0), &compact(d1)));
    }
    if bubble {
        if let (Some((a, b)), Some(c)) = (size_extent, size_col) {
            alt.push_str(&format!(" Bubble size shows {}, from {} to {}.", t.names[c], compact(a), compact(b)));
        }
    }
    alt.push_str(&format!(" {} series.", info.len()));
    for s in info.iter().take(8) {
        let r_text = s.extra.get("correlation").and_then(serde_json::Value::as_f64).map(|c| format!(", correlation {}", compact(c))).unwrap_or_default();
        alt.push_str(&format!(" {:?}: {} points, y from {} to {}{r_text}.", s.name, s.points, s.min.map_or("n/a".into(), compact), s.max.map_or("n/a".into(), compact)));
    }
    if info.len() > 8 {
        alt.push_str(&format!(" And {} more series.", info.len() - 8));
    }
    Ok(Built { svg, series: info, alt })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_of_perfect_lines_and_of_noise() {
        let up: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), 2.0 * f64::from(i) + 1.0)).collect();
        let down: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), -3.0 * f64::from(i))).collect();
        assert!((correlation(&up).unwrap() - 1.0).abs() < 1e-12);
        assert!((correlation(&down).unwrap() + 1.0).abs() < 1e-12);
        let sq: Vec<(f64, f64)> = (-5..=5).map(|i| (f64::from(i), f64::from(i * i))).collect();
        assert!(correlation(&sq).unwrap().abs() < 1e-12, "a symmetric parabola is uncorrelated");
    }

    #[test]
    fn correlation_needs_three_points_and_some_spread() {
        assert_eq!(correlation(&[(1.0, 1.0), (2.0, 2.0)]), None);
        assert_eq!(correlation(&[(1.0, 1.0), (1.0, 2.0), (1.0, 3.0)]), None);
        assert_eq!(correlation(&[(1.0, 5.0), (2.0, 5.0), (3.0, 5.0)]), None);
    }

    #[test]
    fn a_known_small_sample_has_the_textbook_correlation() {
        // x = 1..5, y = 2, 4, 5, 4, 5: r = 0.7745966692414834 (sqrt(0.6)).
        let p = [(1.0, 2.0), (2.0, 4.0), (3.0, 5.0), (4.0, 4.0), (5.0, 5.0)];
        assert!((correlation(&p).unwrap() - 0.6_f64.sqrt()).abs() < 1e-12);
    }
}
