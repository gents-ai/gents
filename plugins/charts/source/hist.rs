//! Histograms: counts of one numeric column per value range, one series per
//! value of the series column drawn over each other on shared bins.

use serde_json::json;

use crate::cols::{self, ColKind};
use crate::common::{self, Built, SeriesInfo, YRange, intro, label_or};
use crate::ctx::Ctx;
use crate::err::{Res, fail};
use crate::format::compact;
use crate::frame::{self, FrameSpec, Laid, LegendItem, Swatch};
use crate::num::round_to;
use crate::spec::{Legend, MAX_SERIES};
use crate::stats::{bin_edges, histogram};
use crate::svg::{Style, Svg};
use crate::table::Table;

/// Draws the chart.
pub fn render(ctx: &mut Ctx<'_>, t: &Table) -> Res<Built> {
    let spec = ctx.spec;
    let col = match &spec.x {
        Some(n) => cols::column(t, n)?,
        None => match (0..t.names.len()).find(|c| cols::kind(t, *c) == ColKind::Numeric) {
            Some(c) => c,
            None => return fail("no column of numbers to count; name it in x"),
        },
    };
    let name = t.names[col].clone();
    let values = cols::numeric(t, col);
    cols::report_numeric(&mut ctx.notes, &name, &values);
    if spec.y_log {
        return fail("a histogram counts values, so its y axis is linear; remove y_scale");
    }
    let series_col = spec
        .series
        .as_deref()
        .map(|n| cols::column(t, n))
        .transpose()?;
    // Values per series.
    let mut groups: Vec<(String, Vec<f64>)> = Vec::new();
    match series_col {
        None => groups.push((
            name.clone(),
            values.v.iter().copied().filter(|v| !v.is_nan()).collect(),
        )),
        Some(sc) => {
            let labels = cols::labels(t, sc);
            let (names, idx) = cols::distinct(&labels);
            let mut per: Vec<Vec<f64>> = vec![Vec::new(); names.len()];
            for (row, g) in idx.iter().enumerate() {
                if let (Some(g), v) = (g, values.v[row])
                    && !v.is_nan()
                {
                    per[*g].push(v);
                }
            }
            if names.len() > MAX_SERIES {
                ctx.notes.add(format!(
                    "column {:?} has {} values; only the first {MAX_SERIES} series are drawn",
                    t.names[sc],
                    names.len()
                ));
            }
            groups.extend(names.into_iter().zip(per).take(MAX_SERIES));
        }
    }
    let all: Vec<f64> = groups.iter().flat_map(|g| g.1.iter().copied()).collect();
    let Some((lo, hi)) = common::range(all.iter().copied()) else {
        return fail("there are no numbers to count; check the column");
    };
    let edges = bin_edges(lo, hi, all.len(), spec.bins);
    let counts: Vec<Vec<u64>> = groups.iter().map(|g| histogram(&g.1, &edges)).collect();
    let tallest = counts.iter().flatten().copied().max().unwrap_or(0).max(1) as f64;
    let x_extent = (edges[0], edges[edges.len() - 1]);

    let legend: Vec<LegendItem> = groups
        .iter()
        .enumerate()
        .map(|(i, g)| LegendItem {
            label: g.0.clone(),
            color: ctx.color(i),
            swatch: Swatch::Box,
        })
        .collect();
    let x_label = label_or(&spec.x_label, &name);
    let y_label = label_or(&spec.y_label, "Count");
    let xf = |target: usize| {
        crate::axes::numeric(
            crate::axes::NumSpec {
                min: x_extent.0,
                max: x_extent.1,
                user_min: spec.x_min,
                user_max: spec.x_max,
                zero: false,
                nice: false,
                log: false,
                integer: false,
            },
            target,
            common::number_format(spec.x_format.as_deref()).as_ref(),
            x_label.clone(),
        )
    };
    let count_fmt = {
        let mut f = crate::format::NumFormat::auto();
        f.digits = Some(0);
        spec.y_format.clone().unwrap_or(f)
    };
    let yf = |target: usize| {
        common::y_axis(
            spec,
            YRange {
                extent: (0.0, tallest),
                zero: true,
                log: false,
                pad: 0.05,
                integer: true,
            },
            target,
            Some(&count_fmt),
            y_label.clone(),
        )
    };
    let Laid { frame, y, .. } = frame::layout(
        ctx,
        &FrameSpec {
            x: &xf,
            y: &yf,
            y2: None,
            legend: &legend,
            extra_right: 0.0,
            auto_legend: Legend::Auto,
        },
    )?;

    let mut svg = Svg::new(ctx.w, ctx.h, ctx.theme.bg);
    frame::draw_title(&mut svg, ctx, &frame);
    frame::draw_grid(&mut svg, ctx, &frame);
    let overlay = groups.len() > 1;
    let base = frame.y.scale.at(0.0);
    let mut info = Vec::new();
    for (i, (g, c)) in groups.iter().zip(&counts).enumerate() {
        let color = ctx.color(i);
        for (b, n) in c.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            let (x0, x1) = (frame.x.scale.at(edges[b]), frame.x.scale.at(edges[b + 1]));
            let top = frame.y.scale.at(*n as f64);
            let tip = format!(
                "{}: {} to {}, count {n}",
                g.0,
                compact(edges[b]),
                compact(edges[b + 1])
            );
            let st = Style::fill(&color)
                .fill_alpha(if overlay { 0.55 } else { 0.9 })
                .outlined(ctx.theme.bg, 1.0);
            svg.rect_titled(
                x0,
                top,
                (x1 - x0).max(1.0),
                (base - top).max(1.0),
                &st,
                &tip,
            );
        }
        let (min, max) = common::range(g.1.iter().copied()).unzip();
        let mut extra = serde_json::Map::new();
        extra.insert(
            "edges".into(),
            json!(edges.iter().map(|e| round_to(*e, 9)).collect::<Vec<_>>()),
        );
        extra.insert("counts".into(), json!(c));
        info.push(SeriesInfo {
            name: g.0.clone(),
            mark: "bar",
            color,
            axis: "left",
            points: g.1.len(),
            min,
            max,
            extra,
            ..SeriesInfo::default()
        });
    }
    frame::draw_axes(&mut svg, ctx, &frame);
    frame::draw_legend(&mut svg, ctx, &frame, &legend);

    let mut alt = intro("Histogram", spec.title.as_deref());
    alt.push_str(&format!(
        " {} values of {name} in {} bins from {} to {}.",
        all.len(),
        edges.len() - 1,
        compact(edges[0]),
        compact(edges[edges.len() - 1])
    ));
    if let Some((d0, d1)) = y.domain() {
        alt.push_str(&format!(
            " Count axis from {} to {}.",
            compact(d0),
            compact(d1)
        ));
    }
    for (g, c) in groups.iter().zip(&counts).take(8) {
        if let Some((k, n)) = c
            .iter()
            .enumerate()
            .max_by_key(|(k, n)| (**n, std::cmp::Reverse(*k)))
        {
            alt.push_str(&format!(
                " {:?}: {} values, tallest bin {} to {} holds {n}.",
                g.0,
                g.1.len(),
                compact(edges[k]),
                compact(edges[k + 1])
            ));
        }
    }
    if groups.len() > 8 {
        alt.push_str(&format!(" And {} more series.", groups.len() - 8));
    }
    Ok(Built {
        svg,
        series: info,
        alt,
        plot: frame.plot,
    })
}
