//! Pie and donut charts.

use serde_json::json;

use crate::cols::{self, ColKind};
use crate::common::{intro, Built, SeriesInfo};
use crate::ctx::Ctx;
use crate::det::{radians, sin_cos};
use crate::err::{fail, Res};
use crate::format::compact;
use crate::frame::{self, LegendItem, Swatch};
use crate::num::round_to;
use crate::palette::{parse_hex, readable_on};
use crate::spec::{Kind, Legend, MAX_SLICES};
use crate::stats::pie_angles;
use crate::svg::{Anchor, PathData, Style, Svg, TextStyle};
use crate::table::Table;

fn point(cx: f64, cy: f64, r: f64, degrees: f64) -> (f64, f64) {
    let (s, c) = sin_cos(radians(degrees));
    (cx + r * s, cy - r * c)
}

/// The outline of the slice between two angles (degrees clockwise from the
/// top); a hole of radius `inner` makes a ring segment.
pub fn slice_path(cx: f64, cy: f64, outer: f64, inner: f64, a0: f64, a1: f64) -> PathData {
    let large = a1 - a0 > 180.0;
    let (p0, p1) = (point(cx, cy, outer, a0), point(cx, cy, outer, a1));
    let mut d = PathData::new();
    if inner <= 0.0 {
        d.move_to(cx, cy).line_to(p0.0, p0.1).arc_to(outer, large, true, p1.0, p1.1).close();
    } else {
        let (q0, q1) = (point(cx, cy, inner, a0), point(cx, cy, inner, a1));
        d.move_to(p0.0, p0.1).arc_to(outer, large, true, p1.0, p1.1).line_to(q1.0, q1.1).arc_to(inner, large, false, q0.0, q0.1).close();
    }
    d
}

fn percent(share: f64) -> String {
    let p = share * 100.0;
    if p >= 10.0 { format!("{p:.0}%") } else { format!("{p:.1}%") }
}

/// Draws the chart.
pub fn render(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    let spec = ctx.spec;
    let donut = spec.kind == Kind::Donut;
    let xc = match &spec.x {
        Some(n) => cols::column(t, n)?,
        None if t.names.is_empty() => return fail("the data has no columns"),
        None => 0,
    };
    let yc = match spec.y.first() {
        Some(n) => cols::column(t, n)?,
        None => match (0..t.names.len()).find(|c| *c != xc && cols::kind(t, *c) == ColKind::Numeric) {
            Some(c) => c,
            None => return fail("no column of numbers for the slices; name it in y"),
        },
    };
    if spec.y.len() > 1 {
        ctx.notes.add("a pie chart uses the first column in y; the others were ignored");
    }
    if spec.series.is_some() {
        ctx.notes.add("series does not apply to pie charts and was ignored");
    }
    let n = cols::numeric(t, yc);
    cols::report_numeric(&mut ctx.notes, &t.names[yc], &n);
    let labels = cols::labels(t, xc);
    let (names, idx) = cols::distinct(&labels);
    let mut buckets: Vec<Vec<f64>> = vec![Vec::new(); names.len()];
    let mut unlabeled = 0usize;
    for (row, g) in idx.iter().enumerate() {
        match g {
            Some(g) => {
                if !n.v[row].is_nan() {
                    buckets[*g].push(n.v[row]);
                }
            }
            None => unlabeled += 1,
        }
    }
    if unlabeled > 0 {
        ctx.notes.add(format!("{unlabeled} rows have no label and are not drawn"));
    }
    let totals: Vec<f64> = buckets.iter().map(|b| cols::aggregate(b, spec.agg)).collect();
    let counts: Vec<usize> = buckets.iter().map(Vec::len).collect();
    let negative = totals.iter().filter(|v| **v < 0.0).count();
    let zero = totals.iter().filter(|v| **v == 0.0).count();
    let none = totals.iter().filter(|v| v.is_nan()).count();
    if negative > 0 {
        ctx.notes.add(format!("{negative} slices have a negative value and are not drawn; a pie shows parts of a whole"));
    }
    if zero + none > 0 {
        ctx.notes.add(format!("{} slices have no positive value and are not drawn", zero + none));
    }
    let usable: Vec<usize> = (0..names.len()).filter(|i| totals[*i] > 0.0).collect();
    if usable.is_empty() {
        return fail("a pie chart needs at least one positive value");
    }
    let usable_names: Vec<String> = usable.iter().map(|i| names[*i].clone()).collect();
    let usable_totals: Vec<f64> = usable.iter().map(|i| totals[*i]).collect();
    let order = cols::order(&usable_names, &usable_totals, spec.sort);
    let mut slices: Vec<(String, f64, usize)> = order.iter().map(|k| (usable_names[*k].clone(), usable_totals[*k], counts[usable[*k]])).collect();
    if slices.len() > MAX_SLICES {
        let mut by_value: Vec<usize> = (0..slices.len()).collect();
        by_value.sort_by(|a, b| slices[*b].1.total_cmp(&slices[*a].1).then(a.cmp(b)));
        let keep: Vec<usize> = by_value[..MAX_SLICES - 1].to_vec();
        let (rest_sum, rest_rows): (f64, usize) = by_value[MAX_SLICES - 1..].iter().fold((0.0, 0), |a, i| (a.0 + slices[*i].1, a.1 + slices[*i].2));
        let rest_n = slices.len() - keep.len();
        let mut kept: Vec<(String, f64, usize)> = slices.iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, s)| s.clone()).collect();
        kept.push(("Other".to_owned(), rest_sum, rest_rows));
        ctx.notes.add(format!("there are {} slices; the {} smallest are grouped as \"Other\"", slices.len(), rest_n));
        slices = kept;
    }
    let values: Vec<f64> = slices.iter().map(|s| s.1).collect();
    let total: f64 = values.iter().sum();
    let angles = pie_angles(&values);

    let items: Vec<LegendItem> = slices
        .iter()
        .enumerate()
        .map(|(i, s)| LegendItem { label: format!("{} ({})", s.0, percent(s.1 / total)), color: ctx.color(i), swatch: Swatch::Box })
        .collect();
    let frame = frame::layout_bare(ctx, &items, Legend::Right, 0.0)?;
    let mut svg = Svg::new(ctx.w, ctx.h, ctx.theme.bg);
    frame::draw_title(&mut svg, ctx, &frame);
    let p = frame.plot;
    let (cx, cy) = (p.x + p.w / 2.0, p.y + p.h / 2.0);
    let outer = (p.w.min(p.h) / 2.0 - 2.0).max(10.0);
    let inner = if donut { outer * 0.58 } else { 0.0 };
    let mut info = Vec::new();
    let fmt = |v: f64| match &spec.y_format {
        Some(f) => f.apply(v, 2),
        None => compact(v),
    };
    for (i, (s, (a0, a1))) in slices.iter().zip(&angles).enumerate() {
        let color = ctx.color(i);
        let share = s.1 / total;
        let tip = format!("{}: {} ({})", s.0, fmt(s.1), percent(share));
        let full = a1 - a0 >= 359.999;
        if full && !donut {
            svg.circle_titled(cx, cy, outer, &Style::fill(&color), &tip);
        } else if full {
            let mid = (outer + inner) / 2.0;
            svg.circle_titled(cx, cy, mid, &Style::stroke(&color, outer - inner), &tip);
        } else {
            svg.path_titled(&slice_path(cx, cy, outer, inner, *a0, *a1), &Style::fill(&color).outlined(ctx.theme.bg, 1.5), &tip);
        }
        if share >= 0.05 {
            let rmid = if donut { (outer + inner) / 2.0 } else { outer * 0.66 };
            let (lx, ly) = point(cx, cy, rmid, (a0 + a1) / 2.0);
            let ink = readable_on(parse_hex(&color).unwrap_or((0, 0, 0)));
            svg.text(lx, ly + 4.0, &percent(share), &TextStyle::new(12.0, ink).bold().anchor(Anchor::Middle));
        }
        let mut extra = serde_json::Map::new();
        extra.insert("value".into(), json!(round_to(s.1, 9)));
        extra.insert("share".into(), json!(round_to(share, 6)));
        info.push(SeriesInfo { name: s.0.clone(), mark: "slice", color, axis: "left", points: s.2, min: Some(s.1), max: Some(s.1), extra, ..SeriesInfo::default() });
    }
    if donut {
        svg.text(cx, cy + 2.0, &fmt(total), &TextStyle::new(20.0, ctx.theme.fg).bold().anchor(Anchor::Middle));
        svg.text(cx, cy + 20.0, "total", &TextStyle::new(12.0, ctx.theme.muted).anchor(Anchor::Middle));
    }
    frame::draw_legend(&mut svg, ctx, &frame, &items);

    let mut alt = intro(if donut { "Donut chart" } else { "Pie chart" }, spec.title.as_deref());
    alt.push_str(&format!(" {} slices of {}, total {}.", slices.len(), t.names[yc], compact(total)));
    for (s, _) in slices.iter().zip(&angles).take(12) {
        alt.push_str(&format!(" {:?} {} ({}).", s.0, fmt(s.1), percent(s.1 / total)));
    }
    Ok(Built { svg, series: info, alt })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_on_the_circle_start_at_the_top_and_run_clockwise() {
        let (x, y) = point(100.0, 100.0, 50.0, 0.0);
        assert!((x - 100.0).abs() < 1e-9 && (y - 50.0).abs() < 1e-9);
        let (x, y) = point(100.0, 100.0, 50.0, 90.0);
        assert!((x - 150.0).abs() < 1e-9 && (y - 100.0).abs() < 1e-9, "three o'clock at 90 degrees");
        let (x, y) = point(100.0, 100.0, 50.0, 180.0);
        assert!((x - 100.0).abs() < 1e-9 && (y - 150.0).abs() < 1e-9);
        let (x, y) = point(100.0, 100.0, 50.0, 270.0);
        assert!((x - 50.0).abs() < 1e-9 && (y - 100.0).abs() < 1e-9);
    }

    #[test]
    fn a_quarter_slice_is_a_wedge_with_the_small_arc_flag() {
        let d = slice_path(100.0, 100.0, 50.0, 0.0, 0.0, 90.0);
        assert_eq!(d.as_str(), "M 100,100 L 100,50 A 50,50 0 0 1 150,100 Z");
    }

    #[test]
    fn a_slice_over_half_the_circle_uses_the_large_arc_flag() {
        let d = slice_path(100.0, 100.0, 50.0, 0.0, 0.0, 270.0);
        assert!(d.as_str().contains("A 50,50 0 1 1 50,100"), "{}", d.as_str());
    }

    #[test]
    fn a_ring_segment_returns_along_the_inner_circle() {
        let d = slice_path(100.0, 100.0, 50.0, 25.0, 0.0, 90.0);
        assert_eq!(d.as_str(), "M 100,50 A 50,50 0 0 1 150,100 L 125,100 A 25,25 0 0 0 100,75 Z");
    }

    #[test]
    fn percentages_keep_one_decimal_below_ten_percent() {
        assert_eq!(percent(0.256), "26%");
        assert_eq!(percent(0.034), "3.4%");
        assert_eq!(percent(1.0), "100%");
        assert_eq!(percent(0.1), "10%");
    }
}
