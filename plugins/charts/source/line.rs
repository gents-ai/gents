//! Line, area and stacked area charts.

use crate::cols;
use crate::common::{self, Built, SeriesInfo, YRange, extremes, intro, label_or};
use crate::ctx::Ctx;
use crate::err::{Res, fail};
use crate::format::compact;
use crate::frame::{self, FrameSpec, Laid, LegendItem, Swatch};
use crate::marks::{area_path, draw_lines, split_runs};
use crate::reduce::lttb;
use crate::shape::{self, XKind, XPolicy};
use crate::spec::{Agg, Kind, MAX_LINE_POINTS};
use crate::stats::stack;
use crate::svg::{Style, Svg};
use crate::table::Table;

struct S {
    name: String,
    color: String,
    pts: Vec<(f64, f64)>,
}

/// Collapses neighbouring points with the same x by `agg`.
fn collapse(pts: &[(f64, f64)], agg: Agg) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut i = 0;
    while i < pts.len() {
        let mut j = i;
        let mut ys = Vec::new();
        while j < pts.len() && pts[j].0 == pts[i].0 {
            if !pts[j].1.is_nan() {
                ys.push(pts[j].1);
            }
            j += 1;
        }
        out.push((pts[i].0, cols::aggregate(&ys, agg)));
        i = j;
    }
    out
}

/// The distinct x values of all series, ascending.
fn union_x(series: &[Vec<(f64, f64)>]) -> Vec<f64> {
    let mut xs: Vec<f64> = series.iter().flat_map(|s| s.iter().map(|p| p.0)).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    xs
}

/// The x positions a stack is drawn at when `xs` has more than
/// [`MAX_LINE_POINTS`]: the largest-triangle picks (first, last, lowest and
/// highest included) of every layer's upper boundary, each layer taking an
/// equal share of the cap. Layers are accumulated one at a time, as
/// [`stack`] does, so no series-by-position matrix is allocated; picking from
/// the totals alone would lose a spike that another layer offsets.
fn reduce_positions(xs: &[f64], data: &[Vec<(f64, f64)>]) -> Vec<f64> {
    let mut pos = vec![0.0; xs.len()];
    let mut neg = vec![0.0; xs.len()];
    let share = MAX_LINE_POINTS / data.len().max(1);
    let mut keep = Vec::new();
    let mut boundary = Vec::with_capacity(xs.len());
    for d in data {
        let mut values = d.iter().peekable();
        boundary.clear();
        for (i, &x) in xs.iter().enumerate() {
            let v = match values.next_if(|p| p.0 == x) {
                Some(p) if !p.1.is_nan() => p.1,
                _ => 0.0,
            };
            let base = if v >= 0.0 { &mut pos[i] } else { &mut neg[i] };
            *base += v;
            boundary.push((x, *base));
        }
        keep.extend(lttb(&boundary, share));
    }
    keep.sort_unstable();
    keep.dedup();
    keep.into_iter().map(|i| xs[i]).collect()
}

/// Draws the chart.
pub fn render(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    let spec = ctx.spec;
    let kind = spec.kind;
    let r = shape::resolve(ctx, t, XPolicy::Auto, &spec.y, &[])?;
    if spec.y_log && kind != Kind::Line {
        return fail(
            "a logarithmic y axis works for line charts; use a linear axis for area charts",
        );
    }
    if r.x.kind == XKind::Num && spec.x_scale == crate::spec::ScaleKind::Log {
        let xs: Vec<f64> = r.x.v.iter().copied().filter(|v| !v.is_nan()).collect();
        common::check_log("x", &r.x.name, &xs)?;
    }
    // One list of (x, y) points per series, ascending in x, gaps as NaN.
    let mut data: Vec<Vec<(f64, f64)>> = if r.x.kind == XKind::Cat {
        r.grid(t, spec.agg)
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .enumerate()
                    .map(|(i, v)| (i as f64, v))
                    .collect()
            })
            .collect()
    } else {
        r.groups
            .iter()
            .map(|g| {
                let mut p = r.points(g, t.rows);
                p.sort_by(|a, b| a.0.total_cmp(&b.0));
                p
            })
            .collect()
    };
    if spec.y_log {
        for (g, d) in r.groups.iter().zip(&data) {
            let ys: Vec<f64> = d.iter().map(|p| p.1).collect();
            common::check_log("y", &g.name, &ys)?;
        }
    }
    let names: Vec<String> = r.groups.iter().map(|g| g.name.clone()).collect();
    let mut layers = Vec::new();
    let mut aligned_x: Vec<f64> = Vec::new();
    if kind == Kind::StackedArea {
        if r.x.kind != XKind::Cat {
            for d in &mut data {
                *d = collapse(d, spec.agg);
            }
        }
        aligned_x = if r.x.kind == XKind::Cat {
            (0..r.x.cats.len()).map(|i| i as f64).collect()
        } else {
            union_x(&data)
        };
        let positions = aligned_x.len();
        for (name, d) in names.iter().zip(&data) {
            let missing = positions - d.iter().filter(|p| !p.1.is_nan()).count();
            if missing > 0 {
                ctx.notes.add(format!("series {name} has no value at {missing} of {positions} positions; it counts as zero there in the stack", name = crate::text::quote(name)));
            }
        }
        if positions > MAX_LINE_POINTS {
            aligned_x = reduce_positions(&aligned_x, &data);
            ctx.notes.add(format!(
                "the stack has {positions} x positions; {} are drawn, chosen by largest-triangle reduction of the stack's top and bottom, which keeps the first, last, lowest and highest",
                aligned_x.len()
            ));
        }
        let matrix: Vec<Vec<f64>> = data
            .iter()
            .map(|d| {
                aligned_x
                    .iter()
                    .map(|x| {
                        d.binary_search_by(|p| p.0.total_cmp(x))
                            .map_or(f64::NAN, |i| d[i].1)
                    })
                    .collect()
            })
            .collect();
        layers = stack(&matrix);
    }

    let x_extent = if r.x.kind == XKind::Cat {
        (0.0, 1.0)
    } else {
        common::range(data.iter().flat_map(|d| d.iter().map(|p| p.0)))
            .ok_or("there are no points to draw")?
    };
    let y_extent = if kind == Kind::StackedArea {
        common::range(
            layers
                .iter()
                .flat_map(|l| l.lo.iter().chain(&l.hi).copied()),
        )
    } else {
        common::range(data.iter().flat_map(|d| d.iter().map(|p| p.1)))
    }
    .ok_or("there are no numbers to draw; check the value column")?;

    for (i, name) in names.iter().enumerate() {
        let ys: Box<dyn Iterator<Item = f64> + '_> = if kind == Kind::StackedArea {
            Box::new(layers[i].hi.iter().copied())
        } else {
            Box::new(data[i].iter().map(|p| p.1))
        };
        common::note_cut(&mut ctx.notes, name, "y", ys, spec.y_min, spec.y_max);
        if r.x.kind != XKind::Cat {
            let xs = data[i].iter().map(|p| p.0);
            common::note_cut(&mut ctx.notes, name, "x", xs, spec.x_min, spec.x_max);
        }
    }

    let legend: Vec<LegendItem> = names
        .iter()
        .enumerate()
        .map(|(i, n)| LegendItem {
            label: n.clone(),
            color: ctx.color(i),
            swatch: if kind == Kind::Line {
                Swatch::Line
            } else {
                Swatch::Box
            },
        })
        .collect();
    let y_label = label_or(&spec.y_label, &r.y_name);
    let zero = kind != Kind::Line;
    let xf = |target: usize| common::x_axis(spec, &r.x, x_extent, target, false);
    let yf = |target: usize| {
        common::y_axis(
            spec,
            YRange {
                extent: y_extent,
                zero,
                log: spec.y_log,
                pad: 0.04,
                integer: false,
            },
            target,
            spec.y_format.as_ref(),
            y_label.clone(),
        )
    };
    let Laid { frame, x, y, .. } = frame::layout(
        ctx,
        &FrameSpec {
            x: &xf,
            y: &yf,
            y2: None,
            legend: &legend,
            extra_right: 0.0,
            auto_legend: crate::spec::Legend::Auto,
        },
    )?;

    let mut svg = Svg::new(ctx.w, ctx.h, ctx.theme.bg);
    frame::draw_title(&mut svg, ctx, &frame);
    frame::draw_grid(&mut svg, ctx, &frame);
    let p = frame.plot;
    let clip = (spec.y_min.is_some()
        || spec.y_max.is_some()
        || spec.x_min.is_some()
        || spec.x_max.is_some())
    .then(|| svg.clip_rect(p.x, p.y, p.w, p.h));
    if let Some(c) = &clip {
        svg.open_group(c);
    }
    let fx = |v: f64| frame.x.scale.at(v);
    let fy = |v: f64| frame.y.scale.at(v);
    let base = match y.domain() {
        Some((d0, d1)) if !spec.y_log && d0 <= 0.0 && d1 >= 0.0 => fy(0.0),
        _ => p.bottom(),
    };

    let mut info = Vec::new();
    let mut summaries = Vec::new();
    if kind == Kind::StackedArea {
        for (i, layer) in layers.iter().enumerate() {
            let color = ctx.color(i);
            let top: Vec<(f64, f64)> = aligned_x
                .iter()
                .zip(&layer.hi)
                .map(|(x, y)| (fx(*x), fy(*y)))
                .collect();
            let bottom: Vec<(f64, f64)> = aligned_x
                .iter()
                .zip(&layer.lo)
                .map(|(x, y)| (fx(*x), fy(*y)))
                .collect();
            let mut d = crate::svg::PathData::new();
            for (k, q) in top.iter().enumerate() {
                if k == 0 {
                    d.move_to(q.0, q.1);
                } else {
                    d.line_to(q.0, q.1);
                }
            }
            for q in bottom.iter().rev() {
                d.line_to(q.0, q.1);
            }
            d.close();
            svg.path(
                &d,
                &Style::fill(&color)
                    .fill_alpha(0.85)
                    .outlined(ctx.theme.bg, 1.0),
            );
            let ys: Vec<(f64, f64)> = data[i].clone();
            let (min, max) = common::range(ys.iter().map(|p| p.1)).unzip();
            info.push(SeriesInfo {
                name: names[i].clone(),
                mark: "area",
                color,
                axis: "left",
                points: ys.iter().filter(|p| p.1.is_finite()).count(),
                drawn: aligned_x.len(),
                gaps: ys.iter().filter(|p| p.1.is_nan()).count(),
                min,
                max,
                ..SeriesInfo::default()
            });
            summaries.push(extremes(&ys));
        }
    } else {
        for (i, d) in data.iter_mut().enumerate() {
            let s = S {
                name: names[i].clone(),
                color: ctx.color(i),
                pts: std::mem::take(d),
            };
            let px: Vec<(f64, f64)> = s
                .pts
                .iter()
                .map(|q| (fx(q.0), if q.1.is_nan() { f64::NAN } else { fy(q.1) }))
                .collect();
            let finite = px.iter().filter(|q| !q.1.is_nan()).count();
            let keep: Vec<usize> = if finite > MAX_LINE_POINTS {
                lttb(&px, MAX_LINE_POINTS)
            } else {
                (0..px.len()).collect()
            };
            if finite > MAX_LINE_POINTS {
                ctx.notes.add(format!(
                    "series {} has {finite} points; {} are drawn, chosen by largest-triangle reduction, which keeps the first, last, lowest and highest points",
                    crate::text::quote(&s.name),
                    keep.len()
                ));
            }
            let runs = split_runs(&px, &keep);
            if kind == Kind::Area {
                for run in &runs {
                    svg.path(
                        &area_path(run, base),
                        &Style::fill(&s.color).fill_alpha(0.28),
                    );
                }
            }
            draw_lines(&mut svg, &runs, &s.color, 2.0);
            if finite <= 50 {
                for (q, orig) in px.iter().zip(&s.pts) {
                    if !q.1.is_nan() {
                        let tip = format!(
                            "{}: {}, {}",
                            s.name,
                            common::x_text(&r.x, orig.0),
                            compact(orig.1)
                        );
                        svg.circle_titled(
                            q.0,
                            q.1,
                            3.0,
                            &Style::fill(&s.color).outlined(ctx.theme.bg, 1.0),
                            &tip,
                        );
                    }
                }
            }
            let (min, max) = common::range(s.pts.iter().map(|q| q.1)).unzip();
            info.push(SeriesInfo {
                name: s.name.clone(),
                mark: if kind == Kind::Area { "area" } else { "line" },
                color: s.color.clone(),
                axis: "left",
                points: finite,
                drawn: keep.iter().filter(|k| !px[**k].1.is_nan()).count(),
                gaps: s.pts.iter().filter(|q| q.1.is_nan()).count(),
                min,
                max,
                ..SeriesInfo::default()
            });
            summaries.push(extremes(&s.pts));
            *d = s.pts;
        }
    }
    if clip.is_some() {
        svg.close_group();
    }
    frame::draw_axes(&mut svg, ctx, &frame);
    frame::draw_legend(&mut svg, ctx, &frame, &legend);

    // Description.
    let kind_name = match kind {
        Kind::Line => "Line chart",
        Kind::Area => "Area chart",
        _ => "Stacked area chart",
    };
    let mut alt = intro(kind_name, spec.title.as_deref());
    alt.push(' ');
    let x_range = match r.x.kind {
        XKind::Cat => (
            r.x.cats.first().cloned().unwrap_or_default(),
            r.x.cats.last().cloned().unwrap_or_default(),
        ),
        _ => common::x_range_text(spec, &r.x, x_extent),
    };
    alt.push_str(&common::axis_sentence(
        "X",
        x.label.as_deref(),
        &x_range.0,
        &x_range.1,
    ));
    if let Some((d0, d1)) = y.domain() {
        alt.push(' ');
        alt.push_str(&common::axis_sentence(
            "Y",
            y.label.as_deref(),
            &compact(d0),
            &compact(d1),
        ));
    }
    alt.push_str(&format!(" {} series.", info.len()));
    for (s, ext) in info.iter().zip(&summaries).take(8) {
        match ext {
            Some((lo, hi)) => alt.push_str(&format!(
                " {}: {} points, lowest {} at {}, highest {} at {}.",
                crate::text::quote(&s.name),
                s.points,
                compact(lo.1),
                common::x_text(&r.x, lo.0),
                compact(hi.1),
                common::x_text(&r.x, hi.0)
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
