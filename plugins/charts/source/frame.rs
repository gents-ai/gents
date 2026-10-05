//! The frame around every chart: title, subtitle, axes with their ticks and
//! labels, grid and legend, and the plot rectangle that is left. Layout is
//! computed from measured text, so labels never collide: category labels
//! rotate, shorten (with the full text in a tooltip) or thin out, and a
//! numeric axis gets fewer ticks until its labels clear each other.

use crate::ctx::Ctx;
use crate::scale::{Band, Scale};
use crate::spec::Legend;
use crate::svg::{Anchor, Style, Svg, TextStyle};
use crate::text::{elide, width};

/// Outer margin.
pub const PAD: f64 = 16.0;
/// Tick label size.
pub const TICK_SIZE: f64 = 11.0;
/// Axis title size.
pub const LABEL_SIZE: f64 = 12.0;
const TICK_LEN: f64 = 4.0;
const MIN_PLOT: f64 = 60.0;

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Right edge.
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    /// Bottom edge.
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// A tick: a value and its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    /// Data value.
    pub value: f64,
    /// Label text.
    pub label: String,
}

/// What an axis measures.
#[derive(Debug, Clone, PartialEq)]
pub enum AxisKind {
    /// A continuous domain with ticks.
    Cont {
        /// Logarithmic.
        log: bool,
        /// Domain start.
        d0: f64,
        /// Domain end.
        d1: f64,
        /// Ticks, ascending.
        ticks: Vec<Tick>,
    },
    /// One slot per label.
    Band {
        /// Slot labels.
        labels: Vec<String>,
    },
}

/// An axis in data terms.
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    /// Kind and domain.
    pub kind: AxisKind,
    /// Title.
    pub label: Option<String>,
}

impl Axis {
    /// The domain of a continuous axis.
    pub fn domain(&self) -> Option<(f64, f64)> {
        match &self.kind {
            AxisKind::Cont { d0, d1, .. } => Some((*d0, *d1)),
            AxisKind::Band { .. } => None,
        }
    }

    fn labels(&self) -> Vec<&str> {
        match &self.kind {
            AxisKind::Cont { ticks, .. } => ticks.iter().map(|t| t.label.as_str()).collect(),
            AxisKind::Band { labels } => labels.iter().map(String::as_str).collect(),
        }
    }
}

/// A legend entry.
#[derive(Debug, Clone, PartialEq)]
pub struct LegendItem {
    /// Text.
    pub label: String,
    /// Colour.
    pub color: String,
    /// Marker shape.
    pub swatch: Swatch,
}

/// Legend marker shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swatch {
    /// A filled square.
    Box,
    /// A short line.
    Line,
    /// A dot.
    Dot,
}

/// A scale along one axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AxisScale {
    /// Continuous.
    Cont(Scale),
    /// Categories.
    Band(Band),
}

impl AxisScale {
    /// Pixel of a continuous value, or of a band centre by index.
    pub fn at(&self, v: f64) -> f64 {
        match self {
            AxisScale::Cont(s) => s.map(v),
            AxisScale::Band(b) => b.center(v as usize),
        }
    }
}

/// A label ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelDraw {
    /// Pixel along the axis.
    pub pos: f64,
    /// Text drawn.
    pub text: String,
    /// Full text when `text` was shortened.
    pub full: Option<String>,
}

/// An axis placed on the canvas.
#[derive(Debug, Clone)]
pub struct AxisLayout {
    /// Scale from data to pixels.
    pub scale: AxisScale,
    /// Labels to draw.
    pub labels: Vec<LabelDraw>,
    /// Label rotation in degrees (0 or -45).
    pub rotate: f64,
    /// Gridline pixels.
    pub grid: Vec<f64>,
    /// Axis title.
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
struct TextLine {
    text: String,
    full: Option<String>,
    size: f64,
    bold: bool,
    y: f64,
}

/// A legend entry placed on the canvas.
#[derive(Debug, Clone)]
pub struct LegendDraw {
    /// Left edge of the marker.
    pub x: f64,
    /// Top of the row.
    pub y: f64,
    /// Text drawn.
    pub text: String,
    /// Full text when shortened.
    pub full: Option<String>,
    /// Entry index.
    pub item: usize,
}

/// The computed layout.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The plot rectangle.
    pub plot: Rect,
    /// Bottom or horizontal axis.
    pub x: AxisLayout,
    /// Left or vertical axis.
    pub y: AxisLayout,
    /// Right axis.
    pub y2: Option<AxisLayout>,
    title: Vec<TextLine>,
    /// Legend entries placed.
    pub legend: Vec<LegendDraw>,
    x_crowded: bool,
    y_crowded: bool,
    x_title_y: f64,
}

/// The axes and legend of a chart, before they are placed.
pub struct FrameSpec<'a> {
    /// Builds the x axis for a target tick count.
    pub x: &'a dyn Fn(usize) -> Axis,
    /// Builds the y axis for a target tick count.
    pub y: &'a dyn Fn(usize) -> Axis,
    /// Builds the right axis for a target tick count.
    pub y2: Option<&'a dyn Fn(usize) -> Axis>,
    /// Legend entries.
    pub legend: &'a [LegendItem],
    /// Extra width reserved at the right (a colour bar).
    pub extra_right: f64,
    /// The legend placement when the caller asked for `auto`.
    pub auto_legend: Legend,
}

/// An axis with the ticks the layout settled on, for the chart to draw with.
pub struct Laid {
    /// The layout.
    pub frame: Frame,
    /// The x axis.
    pub x: Axis,
    /// The y axis.
    pub y: Axis,
    /// The right axis.
    pub y2: Option<Axis>,
}

fn fit_title(text: &str, avail: f64, sizes: &[f64], bold: bool) -> (String, Option<String>, f64) {
    for s in sizes {
        if width(text, *s, bold) <= avail {
            return (text.to_owned(), None, *s);
        }
    }
    let s = sizes[sizes.len() - 1];
    let (t, cut) = elide(text, avail, s, bold);
    (t, cut.then(|| text.to_owned()), s)
}

fn resolve_legend(ctx: &Ctx<'_>, spec: &FrameSpec<'_>) -> Legend {
    let n = spec.legend.len();
    match ctx.spec.legend {
        Legend::None => Legend::None,
        _ if n == 0 => Legend::None,
        Legend::Auto => {
            if n == 1 && spec.auto_legend == Legend::Auto {
                Legend::None
            } else if spec.auto_legend != Legend::Auto {
                spec.auto_legend
            } else {
                let longest = spec.legend.iter().map(|i| width(&i.label, 12.0, false)).fold(0.0, f64::max);
                if n <= 10 && longest <= 160.0 { Legend::Right } else { Legend::Bottom }
            }
        }
        explicit => explicit,
    }
}

struct LegendBox {
    w: f64,
    h: f64,
    items: Vec<(f64, f64, String, Option<String>, usize)>,
}

fn legend_box(items: &[LegendItem], pos: Legend, avail_w: f64, avail_h: f64) -> LegendBox {
    const ROW: f64 = 18.0;
    const MARK: f64 = 18.0;
    let max_text = if matches!(pos, Legend::Left | Legend::Right) { 190.0 } else { 220.0 };
    let shown: Vec<(String, Option<String>, f64)> = items
        .iter()
        .map(|i| {
            let (t, cut) = elide(&i.label, max_text, 12.0, false);
            let w = width(&t, 12.0, false);
            (t, cut.then(|| i.label.clone()), w)
        })
        .collect();
    if matches!(pos, Legend::Left | Legend::Right) {
        let per_col = ((avail_h / ROW).floor() as usize).max(1);
        let cols = shown.len().div_ceil(per_col);
        let mut out = Vec::new();
        let mut x = 0.0;
        for c in 0..cols {
            let chunk = &shown[c * per_col..((c + 1) * per_col).min(shown.len())];
            let col_w = chunk.iter().map(|s| s.2).fold(0.0, f64::max) + MARK;
            for (r, s) in chunk.iter().enumerate() {
                out.push((x, r as f64 * ROW, s.0.clone(), s.1.clone(), c * per_col + r));
            }
            x += col_w + 16.0;
        }
        let rows = shown.len().min(per_col);
        return LegendBox { w: x - 16.0, h: rows as f64 * ROW, items: out };
    }
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut run = 0.0;
    for (i, s) in shown.iter().enumerate() {
        let w = s.2 + MARK;
        if run + w > avail_w && !rows[rows.len() - 1].is_empty() {
            rows.push(Vec::new());
            run = 0.0;
        }
        if let Some(r) = rows.last_mut() {
            r.push(i);
        }
        run += w + 16.0;
    }
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let total: f64 = row.iter().map(|i| shown[*i].2 + MARK).sum::<f64>() + 16.0 * (row.len().saturating_sub(1)) as f64;
        let mut x = (avail_w - total) / 2.0;
        for i in row {
            out.push((x, r as f64 * ROW, shown[*i].0.clone(), shown[*i].1.clone(), *i));
            x += shown[*i].2 + MARK + 16.0;
        }
    }
    LegendBox { w: avail_w, h: rows.len() as f64 * ROW, items: out }
}

/// Labels along a band axis: horizontal when they fit, else rotated and
/// shortened, else thinned out. Returns (rotation, stride, label height,
/// shortened-text flags per kept label).
fn band_x_plan(labels: &[String], slot: f64) -> (f64, usize, f64, Vec<(String, Option<String>)>) {
    let widths: Vec<f64> = labels.iter().map(|l| width(l, TICK_SIZE, false)).collect();
    let longest = widths.iter().copied().fold(0.0, f64::max);
    let draw = |max_w: f64, stride: usize| -> Vec<(String, Option<String>)> {
        labels
            .iter()
            .enumerate()
            .filter(|(i, _)| i % stride == 0)
            .map(|(_, l)| {
                let (t, cut) = elide(l, max_w, TICK_SIZE, false);
                (t, cut.then(|| l.clone()))
            })
            .collect()
    };
    if longest + 8.0 <= slot {
        return (0.0, 1, 15.0, draw(f64::MAX, 1));
    }
    if slot >= 56.0 {
        let max_w = slot - 8.0;
        let shown = draw(max_w, 1);
        return (0.0, 1, 15.0, shown);
    }
    let rot_cap = 150.0;
    let rot_w = longest.min(rot_cap);
    let height = rot_w * 0.7071 + 10.0;
    let stride = (20.0 / slot.max(1.0)).ceil().max(1.0) as usize;
    (-45.0, stride, height, draw(rot_cap, stride))
}

/// Computes the layout for a chart, reducing tick counts until labels clear
/// each other.
pub fn layout(ctx: &mut Ctx<'_>, spec: &FrameSpec<'_>) -> crate::err::Res<Laid> {
    let mut xt = (((ctx.w - 150.0) / 90.0).floor() as usize).clamp(2, 12);
    let mut yt = (((ctx.h - 140.0) / 55.0).floor() as usize).clamp(2, 10);
    let mut guard = 0;
    loop {
        let x = (spec.x)(xt);
        let y = (spec.y)(yt);
        let y2 = spec.y2.map(|f| f(yt));
        let frame = plan(ctx, spec, &x, &y, y2.as_ref())?;
        guard += 1;
        let mut changed = false;
        if frame.x_crowded && xt > 2 {
            xt -= 1;
            changed = true;
        }
        if frame.y_crowded && yt > 2 {
            yt -= 1;
            changed = true;
        }
        if !changed || guard > 24 {
            return Ok(Laid { frame, x, y, y2 });
        }
    }
}

/// The layout of a chart with no axes (a pie): title, legend and the plot
/// rectangle that is left.
pub fn layout_bare(ctx: &mut Ctx<'_>, legend: &[LegendItem], auto_legend: Legend, extra_right: f64) -> crate::err::Res<Frame> {
    let empty = |_: usize| Axis { kind: AxisKind::Cont { log: false, d0: 0.0, d1: 1.0, ticks: Vec::new() }, label: None };
    let spec = FrameSpec { x: &empty, y: &empty, y2: None, legend, extra_right, auto_legend };
    Ok(layout(ctx, &spec)?.frame)
}

fn plan(ctx: &mut Ctx<'_>, spec: &FrameSpec<'_>, x: &Axis, y: &Axis, y2: Option<&Axis>) -> crate::err::Res<Frame> {
    let (w, h) = (ctx.w, ctx.h);
    let avail_w = w - 2.0 * PAD;
    let mut title = Vec::new();
    let mut cursor = PAD;
    if let Some(t) = &ctx.spec.title {
        let (text, full, size) = fit_title(t, avail_w, &[18.0, 16.0, 14.0], true);
        if full.is_some() {
            ctx.notes.add("the title is too long for the image and was shortened; the full text is in the description");
        }
        title.push(TextLine { text, full, size, bold: true, y: cursor + size * 0.9 });
        cursor += size + 6.0;
    }
    if let Some(t) = &ctx.spec.subtitle {
        let (text, full, size) = fit_title(t, avail_w, &[13.0, 12.0, 11.0], false);
        if full.is_some() {
            ctx.notes.add("the subtitle is too long for the image and was shortened; the full text is in the description");
        }
        title.push(TextLine { text, full, size, bold: false, y: cursor + size * 0.85, });
        cursor += size + 6.0;
    }
    if !title.is_empty() {
        cursor += 6.0;
    }
    let mut top = cursor;
    let mut bottom = h - PAD;
    let mut left = PAD;
    let mut right = w - PAD - spec.extra_right;

    let pos = resolve_legend(ctx, spec);
    let mut legend = Vec::new();
    if pos != Legend::None {
        let side_h = h - top - PAD;
        let b = legend_box(spec.legend, pos, avail_w, side_h);
        match pos {
            Legend::Right => {
                let x0 = right - b.w;
                legend.extend(b.items.iter().map(|(dx, dy, t, f, i)| LegendDraw { x: x0 + dx, y: top + dy, text: t.clone(), full: f.clone(), item: *i }));
                right = x0 - 14.0;
            }
            Legend::Left => {
                legend.extend(b.items.iter().map(|(dx, dy, t, f, i)| LegendDraw { x: left + dx, y: top + dy, text: t.clone(), full: f.clone(), item: *i }));
                left += b.w + 14.0;
            }
            Legend::Top => {
                legend.extend(b.items.iter().map(|(dx, dy, t, f, i)| LegendDraw { x: PAD + dx, y: top + dy, text: t.clone(), full: f.clone(), item: *i }));
                top += b.h + 8.0;
            }
            _ => {
                let y0 = bottom - b.h;
                legend.extend(b.items.iter().map(|(dx, dy, t, f, i)| LegendDraw { x: PAD + dx, y: y0 + dy, text: t.clone(), full: f.clone(), item: *i }));
                bottom = y0 - 10.0;
            }
        }
    }

    // Vertical axis widths.
    let side_label = |a: &Axis| if a.label.is_some() { 16.0 } else { 0.0 };
    let band_cap = (w * 0.3).min(200.0);
    let label_w = |a: &Axis| -> f64 {
        let widest = a.labels().iter().map(|l| width(l, TICK_SIZE, false)).fold(0.0, f64::max);
        match a.kind {
            AxisKind::Band { .. } => widest.min(band_cap),
            AxisKind::Cont { .. } => widest,
        }
    };
    let y_w = label_w(y) + TICK_LEN + 5.0;
    let x_is_band = matches!(x.kind, AxisKind::Band { .. });
    // A numeric x axis centres its end labels on the ticks: leave room for them.
    let half_first = if x_is_band { 0.0 } else { x.labels().first().map_or(0.0, |l| width(l, TICK_SIZE, false) / 2.0) };
    let half_last = if x_is_band { 0.0 } else { x.labels().last().map_or(0.0, |l| width(l, TICK_SIZE, false) / 2.0) };
    let mut plot_x = left + side_label(y) + y_w;
    plot_x = plot_x.max(left + half_first);
    if let Some(a2) = y2 {
        right -= label_w(a2) + TICK_LEN + 5.0 + side_label(a2);
    }
    right = right.min(w - PAD - half_last + 0.0).max(plot_x);
    let plot_w = right - plot_x;

    // Horizontal axis height.
    let x_labels: Vec<(String, Option<String>)>;
    let (mut rotate, mut stride, mut x_h) = (0.0, 1, 15.0);
    let x_title_h = if x.label.is_some() { 18.0 } else { 0.0 };
    match &x.kind {
        AxisKind::Band { labels } => {
            let slot = plot_w / labels.len().max(1) as f64;
            (rotate, stride, x_h, x_labels) = band_x_plan(labels, slot);
        }
        AxisKind::Cont { ticks, .. } => {
            x_labels = ticks.iter().map(|t| (t.label.clone(), None)).collect();
        }
    }
    let x_axis_h = TICK_LEN + 4.0 + x_h;
    let plot_bottom = bottom - x_title_h - x_axis_h;
    let plot_y = top + 4.0;
    let plot_h = plot_bottom - plot_y;
    if plot_w < MIN_PLOT || plot_h < MIN_PLOT {
        return crate::err::fail("the chart is too small for its labels and legend; make it larger, shorten the labels or move the legend");
    }
    let plot = Rect { x: plot_x, y: plot_y, w: plot_w, h: plot_h };

    // Scales and label placement.
    let xs = make_scale(x, plot.x, plot.right(), false, plot);
    let ys = make_scale(y, plot.bottom(), plot.y, true, plot);
    let mut x_crowded = false;
    let mut y_crowded = false;

    let mut x_draw = Vec::new();
    match (&x.kind, &xs) {
        (AxisKind::Cont { .. }, AxisScale::Cont(s)) => {
            let AxisKind::Cont { ticks, .. } = &x.kind else { unreachable!() };
            let mut prev_end = f64::NEG_INFINITY;
            for (t, (text, _)) in ticks.iter().zip(&x_labels) {
                let pos = s.map(t.value);
                let wd = width(text, TICK_SIZE, false);
                if pos - wd / 2.0 < prev_end + 6.0 {
                    x_crowded = true;
                }
                prev_end = pos + wd / 2.0;
                x_draw.push(LabelDraw { pos, text: text.clone(), full: None });
            }
        }
        (AxisKind::Band { labels }, AxisScale::Band(b)) => {
            let n = labels.len();
            let mut shown = x_labels.iter();
            for i in (0..n).step_by(stride) {
                if let Some((text, full)) = shown.next() {
                    x_draw.push(LabelDraw { pos: b.center(i), text: text.clone(), full: full.clone() });
                }
            }
            if stride > 1 {
                ctx.notes.add(format!("only every {stride}th of {n} category labels is shown so they do not overlap; all values are drawn"));
            }
        }
        _ => {}
    }
    let mut y_draw = Vec::new();
    let place_y = |a: &Axis, s: &AxisScale, crowded: &mut bool, notes: &mut crate::cols::Notes| -> Vec<LabelDraw> {
        let mut out = Vec::new();
        match (&a.kind, s) {
            (AxisKind::Cont { ticks, .. }, AxisScale::Cont(sc)) => {
                let mut prev = f64::INFINITY;
                for t in ticks {
                    let pos = sc.map(t.value);
                    if prev - pos < 15.0 && !out.is_empty() {
                        *crowded = true;
                    }
                    prev = pos;
                    out.push(LabelDraw { pos, text: t.label.clone(), full: None });
                }
            }
            (AxisKind::Band { labels }, AxisScale::Band(b)) => {
                let n = labels.len();
                let slot = (b.step()).abs();
                let stride = (14.0 / slot.max(1.0)).ceil().max(1.0) as usize;
                for i in (0..n).step_by(stride) {
                    let (t, cut) = elide(&labels[i], band_cap, TICK_SIZE, false);
                    out.push(LabelDraw { pos: b.center(i), text: t, full: cut.then(|| labels[i].clone()) });
                }
                if stride > 1 {
                    notes.add(format!("only every {stride}th of {n} category labels is shown so they do not overlap; all values are drawn"));
                }
            }
            _ => {}
        }
        out
    };
    y_draw.extend(place_y(y, &ys, &mut y_crowded, &mut ctx.notes));
    let y2_layout = match y2 {
        Some(a2) => {
            let s2 = make_scale(a2, plot.bottom(), plot.y, true, plot);
            let mut c = false;
            let labels = place_y(a2, &s2, &mut c, &mut ctx.notes);
            y_crowded |= c;
            Some(AxisLayout { scale: s2, labels, rotate: 0.0, grid: Vec::new(), title: a2.label.clone() })
        }
        None => None,
    };

    let grid_of = |a: &Axis, s: &AxisScale| -> Vec<f64> {
        match (&a.kind, s) {
            (AxisKind::Cont { ticks, .. }, AxisScale::Cont(sc)) => ticks.iter().map(|t| sc.map(t.value)).collect(),
            _ => Vec::new(),
        }
    };
    let x_grid = grid_of(x, &xs);
    let y_grid = grid_of(y, &ys);
    let x_title_y = bottom - 4.0;
    Ok(Frame {
        plot,
        x: AxisLayout { scale: xs, labels: x_draw, rotate, grid: x_grid, title: x.label.clone() },
        y: AxisLayout { scale: ys, labels: y_draw, rotate: 0.0, grid: y_grid, title: y.label.clone() },
        y2: y2_layout,
        title,
        legend,
        x_crowded,
        y_crowded,
        x_title_y,
    })
}

fn make_scale(a: &Axis, from: f64, to: f64, vertical_up: bool, plot: Rect) -> AxisScale {
    let _ = (vertical_up, plot);
    match &a.kind {
        AxisKind::Cont { log, d0, d1, .. } => AxisScale::Cont(if *log { Scale::log(*d0, *d1, from, to) } else { Scale::linear(*d0, *d1, from, to) }),
        AxisKind::Band { labels } => {
            // A band axis on the vertical side lists its first category at the top.
            let (r0, r1) = if vertical_up { (to, from) } else { (from, to) };
            AxisScale::Band(Band { n: labels.len(), r0, r1 })
        }
    }
}

/// Draws the title and subtitle.
pub fn draw_title(svg: &mut Svg, ctx: &Ctx<'_>, f: &Frame) {
    for line in &f.title {
        let fill = if line.bold { ctx.theme.fg } else { ctx.theme.muted };
        let mut st = TextStyle::new(line.size, fill);
        if line.bold {
            st = st.bold();
        }
        if let Some(full) = &line.full {
            st = st.full(full);
        }
        svg.text(PAD, line.y, &line.text, &st);
    }
}

/// Draws the gridlines behind the data.
pub fn draw_grid(svg: &mut Svg, ctx: &Ctx<'_>, f: &Frame) {
    let st = Style::stroke(ctx.theme.grid, 1.0);
    let p = f.plot;
    for y in &f.y.grid {
        svg.line(p.x, y.round() + 0.5, p.right(), y.round() + 0.5, &st);
    }
    for x in &f.x.grid {
        svg.line(x.round() + 0.5, p.y, x.round() + 0.5, p.bottom(), &st);
    }
}

/// Draws the axis lines, ticks, labels and titles over the data.
pub fn draw_axes(svg: &mut Svg, ctx: &Ctx<'_>, f: &Frame) {
    let p = f.plot;
    let line = Style::stroke(ctx.theme.axis, 1.0);
    let muted = ctx.theme.muted;
    svg.line(p.x, p.bottom() + 0.5, p.right(), p.bottom() + 0.5, &line);
    svg.line(p.x - 0.5, p.y, p.x - 0.5, p.bottom(), &line);
    // Horizontal axis.
    for l in &f.x.labels {
        let px = l.pos.round() + 0.5;
        svg.line(px, p.bottom(), px, p.bottom() + TICK_LEN, &line);
        let mut st = TextStyle::new(TICK_SIZE, muted);
        if let Some(full) = &l.full {
            st = st.full(full);
        }
        if f.x.rotate != 0.0 {
            st = st.anchor(Anchor::End).rotate(f.x.rotate);
            svg.text(l.pos + 3.0, p.bottom() + TICK_LEN + 9.0, &l.text, &st);
        } else {
            st = st.anchor(Anchor::Middle);
            svg.text(l.pos, p.bottom() + TICK_LEN + 12.0, &l.text, &st);
        }
    }
    // Vertical axis.
    for l in &f.y.labels {
        let py = l.pos.round() + 0.5;
        svg.line(p.x - TICK_LEN, py, p.x, py, &line);
        let mut st = TextStyle::new(TICK_SIZE, muted).anchor(Anchor::End);
        if let Some(full) = &l.full {
            st = st.full(full);
        }
        svg.text(p.x - TICK_LEN - 4.0, l.pos + 4.0, &l.text, &st);
    }
    if let Some(a2) = &f.y2 {
        svg.line(p.right() + 0.5, p.y, p.right() + 0.5, p.bottom(), &line);
        for l in &a2.labels {
            let py = l.pos.round() + 0.5;
            svg.line(p.right(), py, p.right() + TICK_LEN, py, &line);
            svg.text(p.right() + TICK_LEN + 4.0, l.pos + 4.0, &l.text, &TextStyle::new(TICK_SIZE, muted));
        }
    }
    // Titles.
    if let Some(t) = &f.x.title {
        let (text, cut) = elide(t, p.w, LABEL_SIZE, false);
        let mut st = TextStyle::new(LABEL_SIZE, ctx.theme.fg).anchor(Anchor::Middle);
        if cut {
            st = st.full(t);
        }
        svg.text(p.x + p.w / 2.0, f.x_title_y, &text, &st);
    }
    if let Some(t) = &f.y.title {
        let (text, cut) = elide(t, p.h, LABEL_SIZE, false);
        let mut st = TextStyle::new(LABEL_SIZE, ctx.theme.fg).anchor(Anchor::Middle).rotate(-90.0);
        if cut {
            st = st.full(t);
        }
        let label_w = f.y.labels.iter().map(|l| width(&l.text, TICK_SIZE, false)).fold(0.0, f64::max);
        let bx = p.x - TICK_LEN - 5.0 - label_w - 5.0;
        svg.text(bx, p.y + p.h / 2.0, &text, &st);
    }
    if let Some(a2) = &f.y2 {
        if let Some(t) = &a2.title {
            let (text, cut) = elide(t, p.h, LABEL_SIZE, false);
            let mut st = TextStyle::new(LABEL_SIZE, ctx.theme.fg).anchor(Anchor::Middle).rotate(90.0);
            if cut {
                st = st.full(t);
            }
            let label_w = a2.labels.iter().map(|l| width(&l.text, TICK_SIZE, false)).fold(0.0, f64::max);
            let bx = p.right() + TICK_LEN + 5.0 + label_w + 5.0;
            svg.text(bx, p.y + p.h / 2.0, &text, &st);
        }
    }
}

/// Draws the legend.
pub fn draw_legend(svg: &mut Svg, ctx: &Ctx<'_>, f: &Frame, items: &[LegendItem]) {
    for d in &f.legend {
        let item = &items[d.item];
        let (x, y) = (d.x, d.y);
        match item.swatch {
            Swatch::Box => svg.rect(x, y + 3.0, 12.0, 12.0, &Style::fill(&item.color)),
            Swatch::Line => svg.line(x, y + 9.0, x + 14.0, y + 9.0, &Style::stroke(&item.color, 2.5)),
            Swatch::Dot => svg.circle(x + 7.0, y + 9.0, 5.0, &Style::fill(&item.color)),
        }
        let mut st = TextStyle::new(12.0, ctx.theme.fg);
        if let Some(full) = &d.full {
            st = st.full(full);
        }
        svg.text(x + 20.0, y + 13.0, &d.text, &st);
    }
}
