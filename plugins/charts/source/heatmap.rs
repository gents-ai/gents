//! Heatmaps: a grid of colours for the value of every pair of categories.

use serde_json::json;

use crate::cols::{self, ColKind};
use crate::common::{self, intro, Built, SeriesInfo};
use crate::ctx::Ctx;
use crate::err::{fail, Res};
use crate::format::compact;
use crate::frame::{self, FrameSpec, Laid};
use crate::palette::{diverging, hex, readable_on, sequential, Rgb, Theme};
use crate::scale::nice_ticks;
use crate::spec::{Agg, Legend, MAX_HEAT, Sort};
use crate::svg::{Anchor, Style, Svg, TextStyle};
use crate::table::Table;

/// Maps values to colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Norm {
    /// Lowest value.
    pub lo: f64,
    /// Highest value.
    pub hi: f64,
    /// The neutral value of a diverging scale.
    pub center: Option<f64>,
}

impl Norm {
    /// Position of `v` on the colour ramp, 0 to 1.
    pub fn t(&self, v: f64) -> f64 {
        match self.center {
            Some(c) => {
                let reach = (self.hi - c).abs().max((self.lo - c).abs());
                if reach > 0.0 { 0.5 + 0.5 * (v - c) / reach } else { 0.5 }
            }
            None if self.hi > self.lo => (v - self.lo) / (self.hi - self.lo),
            None => 0.5,
        }
    }

    /// The colour of `v`.
    pub fn color(&self, v: f64, theme: &Theme) -> Rgb {
        let t = self.t(v);
        if self.center.is_some() { diverging(t, theme) } else { sequential(t) }
    }
}

fn axis_labels(t: &Table, col: usize, sort: Sort, totals_by: &dyn Fn(usize) -> f64, ctx: &mut Ctx<'_>, what: &str) -> (Vec<String>, Vec<Option<usize>>) {
    let labels = cols::labels(t, col);
    let (names, idx) = cols::distinct(&labels);
    let totals: Vec<f64> = (0..names.len()).map(totals_by).collect();
    let order = cols::order(&names, &totals, sort);
    let keep = order.len().min(MAX_HEAT);
    if order.len() > keep {
        ctx.notes.add(format!("there are {} {what}; only the first {keep} are drawn", order.len()));
    }
    let mut pos = vec![None; names.len()];
    for (p, o) in order.iter().take(keep).enumerate() {
        pos[*o] = Some(p);
    }
    let shown = order.iter().take(keep).map(|o| names[*o].clone()).collect();
    (shown, idx.iter().map(|i| i.and_then(|i| pos[i])).collect())
}

/// Draws the chart.
pub fn render(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    let spec = ctx.spec;
    if t.names.len() < 3 && (spec.x.is_none() || spec.y.is_empty() || spec.value.is_none()) {
        return fail("a heatmap needs three columns: x, y and value");
    }
    let xc = match &spec.x {
        Some(n) => cols::column(t, n)?,
        None => 0,
    };
    let yc = match spec.y.first() {
        Some(n) => cols::column(t, n)?,
        None => (0..t.names.len()).find(|c| *c != xc).unwrap_or(0),
    };
    let vc = match &spec.value {
        Some(n) => cols::column(t, n)?,
        None => match (0..t.names.len()).find(|c| *c != xc && *c != yc && cols::kind(t, *c) == ColKind::Numeric) {
            Some(c) => c,
            None => return fail("no column of numbers for the cell values; name it in value"),
        },
    };
    if xc == yc {
        return fail("x and y name the same column; a heatmap needs two different columns");
    }
    let nums = cols::numeric(t, vc);
    cols::report_numeric(&mut ctx.notes, &t.names[vc], &nums);
    // Totals along each axis for the value orders.
    let xl = cols::labels(t, xc);
    let yl = cols::labels(t, yc);
    let (xn, xi) = cols::distinct(&xl);
    let (yn, yi) = cols::distinct(&yl);
    let (mut xt, mut yt) = (vec![0.0; xn.len()], vec![0.0; yn.len()]);
    for row in 0..t.rows {
        if let (Some(a), Some(b), v) = (xi[row], yi[row], nums.v[row]) {
            if !v.is_nan() {
                xt[a] += v;
                yt[b] += v;
            }
        }
    }
    let (xcats, xpos) = axis_labels(t, xc, spec.sort, &|i| xt[i], ctx, "x categories");
    let (ycats, ypos) = axis_labels(t, yc, spec.sort, &|i| yt[i], ctx, "y categories");
    let agg = if spec.agg_given { spec.agg } else { Agg::Mean };
    let mut cells: Vec<Vec<Vec<f64>>> = vec![vec![Vec::new(); xcats.len()]; ycats.len()];
    for row in 0..t.rows {
        if let (Some(a), Some(b)) = (xpos[row], ypos[row]) {
            if !nums.v[row].is_nan() {
                cells[b][a].push(nums.v[row]);
            }
        }
    }
    let grid: Vec<Vec<f64>> = cells.iter().map(|r| r.iter().map(|c| cols::aggregate(c, agg)).collect()).collect();
    let Some((lo, hi)) = common::range(grid.iter().flatten().copied()) else {
        return fail("there are no numbers to colour; check the value column");
    };
    let center = match spec.center {
        Some(c) => Some(c),
        None if lo < 0.0 && hi > 0.0 => Some(0.0),
        None => None,
    };
    let norm = Norm { lo, hi, center };
    let filled = grid.iter().flatten().filter(|v| !v.is_nan()).count();
    let empty = xcats.len() * ycats.len() - filled;
    if empty > 0 {
        ctx.notes.add(format!("{empty} cells have no value and are drawn empty"));
    }
    let x_label = common::label_or(&spec.x_label, &t.names[xc]);
    let y_label = common::label_or(&spec.y_label, &t.names[yc]);
    let xf = |_t: usize| crate::axes::band(xcats.clone(), x_label.clone());
    let yf = |_t: usize| crate::axes::band(ycats.clone(), y_label.clone());
    let Laid { frame, .. } = frame::layout(ctx, &FrameSpec { x: &xf, y: &yf, y2: None, legend: &[], extra_right: 84.0, auto_legend: Legend::None })?;

    let mut svg = Svg::new(ctx.w, ctx.h, ctx.theme.bg);
    frame::draw_title(&mut svg, ctx, &frame);
    let p = frame.plot;
    let (cw, ch) = (p.w / xcats.len() as f64, p.h / ycats.len() as f64);
    let show_text = cw >= 34.0 && ch >= 15.0 && xcats.len() * ycats.len() <= 400;
    let fmt = |v: f64| match &spec.y_format {
        Some(f) => f.apply(v, 2),
        None => compact(v),
    };
    for (r, row) in grid.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            let (x, y) = (p.x + c as f64 * cw, p.y + r as f64 * ch);
            if v.is_nan() {
                svg.rect_titled(x + 0.5, y + 0.5, cw - 1.0, ch - 1.0, &Style::fill(ctx.theme.grid).fill_alpha(0.35), &format!("{}, {}: no value", ycats[r], xcats[c]));
                continue;
            }
            let rgb = norm.color(*v, &ctx.theme);
            let tip = format!("{}, {}: {}", ycats[r], xcats[c], fmt(*v));
            svg.rect_titled(x + 0.5, y + 0.5, cw - 1.0, ch - 1.0, &Style::fill(&hex(rgb)), &tip);
            if show_text {
                svg.text(x + cw / 2.0, y + ch / 2.0 + 4.0, &fmt(*v), &TextStyle::new(10.5, readable_on(rgb)).anchor(Anchor::Middle));
            }
        }
    }
    frame::draw_axes(&mut svg, ctx, &frame);

    // Colour bar.
    let bar_h = p.h.min(240.0);
    let bx = p.right() + 16.0;
    let by = p.y + (p.h - bar_h) / 2.0;
    let stops: Vec<(f64, String)> = (0..=10).map(|i| {
        let tt = f64::from(i) / 10.0;
        (tt, hex(norm.color(lo + tt * (hi - lo), &ctx.theme)))
    }).collect();
    svg.vertical_gradient("scale", &stops);
    svg.rect(bx, by, 14.0, bar_h, &Style::fill("url(#scale)"));
    let ticks = nice_ticks(lo, hi, 4);
    for v in ticks.values.iter().filter(|v| **v >= lo && **v <= hi) {
        let ty = by + bar_h - (v - lo) / (hi - lo).max(f64::MIN_POSITIVE) * bar_h;
        svg.line(bx + 14.0, ty, bx + 18.0, ty, &Style::stroke(ctx.theme.axis, 1.0));
        svg.text(bx + 22.0, ty + 3.5, &fmt(*v), &TextStyle::new(10.0, ctx.theme.muted));
    }

    let mut extra = serde_json::Map::new();
    extra.insert("rows".into(), json!(ycats.len()));
    extra.insert("columns".into(), json!(xcats.len()));
    extra.insert("empty_cells".into(), json!(empty));
    let info = vec![SeriesInfo { name: t.names[vc].clone(), mark: "cell", color: hex(norm.color(hi, &ctx.theme)), axis: "left", points: filled, gaps: empty, min: Some(lo), max: Some(hi), extra, ..SeriesInfo::default() }];

    let mut alt = intro("Heatmap", spec.title.as_deref());
    alt.push_str(&format!(" {} columns of {} by {} rows of {}, coloured by {} from {} to {}", xcats.len(), t.names[xc], ycats.len(), t.names[yc], t.names[vc], compact(lo), compact(hi)));
    if let Some(c) = center {
        alt.push_str(&format!(", with {} as the neutral colour", compact(c)));
    }
    alt.push('.');
    let mut best: Option<(usize, usize, f64)> = None;
    let mut worst: Option<(usize, usize, f64)> = None;
    for (r, row) in grid.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            if best.is_none_or(|b| *v > b.2) {
                best = Some((r, c, *v));
            }
            if worst.is_none_or(|b| *v < b.2) {
                worst = Some((r, c, *v));
            }
        }
    }
    if let (Some(b), Some(w)) = (best, worst) {
        alt.push_str(&format!(" Highest {} at {} and {}; lowest {} at {} and {}.", compact(b.2), xcats[b.1], ycats[b.0], compact(w.2), xcats[w.1], ycats[w.0]));
    }
    Ok(Built { svg, series: info, alt })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::LIGHT;

    #[test]
    fn a_sequential_norm_runs_from_the_lowest_to_the_highest_value() {
        let n = Norm { lo: 10.0, hi: 30.0, center: None };
        assert_eq!((n.t(10.0), n.t(20.0), n.t(30.0)), (0.0, 0.5, 1.0));
        assert_eq!(n.color(10.0, &LIGHT), sequential(0.0));
        assert_eq!(n.color(30.0, &LIGHT), sequential(1.0));
    }

    #[test]
    fn a_diverging_norm_puts_the_centre_in_the_middle_and_is_symmetric_about_it() {
        let n = Norm { lo: -2.0, hi: 8.0, center: Some(0.0) };
        assert_eq!(n.t(0.0), 0.5);
        assert_eq!(n.t(8.0), 1.0);
        assert_eq!(n.t(-8.0), 0.0);
        assert!((n.t(-2.0) - 0.375).abs() < 1e-12);
        assert_eq!(n.color(0.0, &LIGHT), diverging(0.5, &LIGHT));
    }

    #[test]
    fn a_flat_range_does_not_divide_by_zero() {
        assert_eq!(Norm { lo: 3.0, hi: 3.0, center: None }.t(3.0), 0.5);
        assert_eq!(Norm { lo: 0.0, hi: 0.0, center: Some(0.0) }.t(0.0), 0.5);
    }
}
