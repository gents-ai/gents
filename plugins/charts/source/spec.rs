//! The request a caller sends and the validated chart specification built
//! from it. Every field is checked once, here, with a one-sentence error.

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::dates;
use crate::err::{Res, fail};
use crate::format::{self, NumFormat};
use crate::palette::{self, Theme};

/// Series drawn at most.
pub const MAX_SERIES: usize = 24;
/// Categories on one axis at most.
pub const MAX_CATEGORIES: usize = 100;
/// Cells on each side of a heatmap at most.
pub const MAX_HEAT: usize = 80;
/// Pie slices before the rest is grouped as "Other".
pub const MAX_SLICES: usize = 12;
/// Box plot groups at most.
pub const MAX_BOXES: usize = 40;
/// Histogram bins at most.
pub const MAX_BINS: usize = 200;
/// Points drawn per series before it is reduced.
pub const MAX_LINE_POINTS: usize = 1500;
/// Scatter points drawn before they are binned.
pub const MAX_SCATTER_POINTS: usize = 10_000;
/// Pixels of one image at most (width x height x scale squared).
pub const MAX_PIXELS: u64 = 16_000_000;
/// Smallest and largest width and height.
pub const MIN_WIDTH: u32 = 200;
/// Smallest height.
pub const MIN_HEIGHT: u32 = 150;
/// Largest width or height.
pub const MAX_SIDE: u32 = 4096;

/// One name or a list of names.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Names {
    /// A single column.
    One(String),
    /// Several columns.
    Many(Vec<String>),
}

impl Names {
    fn into_vec(self) -> Vec<String> {
        match self {
            Names::One(s) => vec![s],
            Names::Many(v) => v,
        }
    }
}

/// `bins`: a count or `auto`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum BinsArg {
    /// An exact number of bins.
    Count(u32),
    /// `auto`.
    Text(String),
}

/// Where to write the image files.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SaveArg {
    /// A base name; `.svg` and `.png` are added.
    Base(String),
    /// Explicit file names.
    Files {
        /// SVG file name.
        svg: Option<String>,
        /// PNG file name.
        png: Option<String>,
    },
}

/// The request as it arrives.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request<'a> {
    /// The chart type.
    #[serde(alias = "type")]
    pub chart: Option<String>,
    /// Inline data: rows and columns, a list of objects, or CSV text.
    #[serde(borrow)]
    pub data: Option<&'a RawValue>,
    /// The data file or folder; set by the host.
    pub path: Option<String>,
    /// The real path of `path`; set by the host.
    pub path_original: Option<String>,
    /// A data file inside the folder `path`.
    pub file: Option<String>,
    /// Column for the x axis or the categories.
    pub x: Option<String>,
    /// Value column or columns.
    pub y: Option<Names>,
    /// Combo charts: value columns drawn as lines.
    pub line: Option<Names>,
    /// Column that splits rows into series.
    pub series: Option<String>,
    /// Bubble size column.
    pub size: Option<String>,
    /// Heatmap value column.
    pub value: Option<String>,
    /// How rows with the same category are combined.
    pub agg: Option<String>,
    /// Category order.
    pub sort: Option<String>,
    /// Bar layout: grouped or stacked.
    pub stack: Option<String>,
    /// Draw bars horizontally.
    pub horizontal: Option<bool>,
    /// Histogram bins.
    pub bins: Option<BinsArg>,
    /// Chart title.
    pub title: Option<String>,
    /// Chart subtitle.
    pub subtitle: Option<String>,
    /// X axis label.
    pub x_label: Option<String>,
    /// Y axis label.
    pub y_label: Option<String>,
    /// Second y axis label.
    pub y2_label: Option<String>,
    /// X axis scale.
    pub x_scale: Option<String>,
    /// Y axis scale.
    pub y_scale: Option<String>,
    /// Second y axis scale.
    pub y2_scale: Option<String>,
    /// Number format for values.
    pub format: Option<String>,
    /// Format for x axis labels.
    pub x_format: Option<String>,
    /// Format for y axis labels.
    pub y_format: Option<String>,
    /// Format for the second y axis labels.
    pub y2_format: Option<String>,
    /// Legend placement.
    pub legend: Option<String>,
    /// Light or dark.
    pub theme: Option<String>,
    /// Series colours.
    pub colors: Option<Vec<String>>,
    /// Combo charts: which axis the lines use.
    pub line_axis: Option<String>,
    /// Image width in pixels.
    pub width: Option<u32>,
    /// Image height in pixels.
    pub height: Option<u32>,
    /// PNG pixel density.
    pub scale: Option<f64>,
    /// Lowest x shown.
    pub x_min: Option<f64>,
    /// Highest x shown.
    pub x_max: Option<f64>,
    /// Lowest y shown.
    pub y_min: Option<f64>,
    /// Highest y shown.
    pub y_max: Option<f64>,
    /// Heatmap: the value drawn as the neutral colour.
    pub center: Option<f64>,
    /// What to return: both, svg or png.
    pub output: Option<String>,
    /// Files to write into the bound folder.
    pub save: Option<SaveArg>,
    /// Set by a graph node.
    pub run_id: Option<String>,
}

/// Chart type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Lines.
    Line,
    /// Filled areas from the baseline.
    Area,
    /// Areas stacked on each other.
    StackedArea,
    /// Bars.
    Bar,
    /// Points.
    Scatter,
    /// Points sized by a column.
    Bubble,
    /// Counts per value range.
    Histogram,
    /// Box and whisker.
    Box,
    /// Pie.
    Pie,
    /// Donut.
    Donut,
    /// Colour grid.
    Heatmap,
    /// Bars with lines on a second axis.
    Combo,
}

impl Kind {
    /// The name callers use.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Line => "line",
            Kind::Area => "area",
            Kind::StackedArea => "stacked_area",
            Kind::Bar => "bar",
            Kind::Scatter => "scatter",
            Kind::Bubble => "bubble",
            Kind::Histogram => "histogram",
            Kind::Box => "box",
            Kind::Pie => "pie",
            Kind::Donut => "donut",
            Kind::Heatmap => "heatmap",
            Kind::Combo => "combo",
        }
    }
}

/// How rows with the same category are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    /// Add.
    Sum,
    /// Average.
    Mean,
    /// Count rows.
    Count,
    /// Smallest.
    Min,
    /// Largest.
    Max,
    /// Middle value.
    Median,
}

/// Category order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// As the data lists them.
    None,
    /// By label, ascending.
    X,
    /// By label, descending.
    XDesc,
    /// By total, ascending.
    Value,
    /// By total, descending.
    ValueDesc,
}

/// Axis scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleKind {
    /// Decide from the data.
    Auto,
    /// Linear numbers.
    Linear,
    /// Logarithmic numbers.
    Log,
    /// Dates and times.
    Time,
    /// Labels.
    Category,
}

/// Legend placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Legend {
    /// Decide from the series.
    Auto,
    /// No legend.
    None,
    /// Right of the plot.
    Right,
    /// Below the plot.
    Bottom,
    /// Above the plot.
    Top,
    /// Left of the plot.
    Left,
}

/// What the call returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Vector and raster.
    Both,
    /// SVG only.
    Svg,
    /// PNG only.
    Png,
}

/// Histogram bins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bins {
    /// Chosen from the data.
    Auto,
    /// This many.
    Count(usize),
}

/// File names to write, as validated relative paths.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SaveSpec {
    /// SVG file.
    pub svg: Option<String>,
    /// PNG file.
    pub png: Option<String>,
}

/// A validated chart specification.
#[derive(Debug, Clone)]
pub struct Spec {
    /// Chart type.
    pub kind: Kind,
    /// X column.
    pub x: Option<String>,
    /// Value columns.
    pub y: Vec<String>,
    /// Line columns of a combo chart.
    pub line: Vec<String>,
    /// Series column.
    pub series: Option<String>,
    /// Size column.
    pub size: Option<String>,
    /// Heatmap value column.
    pub value: Option<String>,
    /// Aggregation.
    pub agg: Agg,
    /// True when the caller named `agg`.
    pub agg_given: bool,
    /// Category order.
    pub sort: Sort,
    /// Stacked bars.
    pub stacked: bool,
    /// Horizontal bars.
    pub horizontal: bool,
    /// Histogram bins.
    pub bins: Bins,
    /// Title.
    pub title: Option<String>,
    /// Subtitle.
    pub subtitle: Option<String>,
    /// X label; empty when the caller asked for none.
    pub x_label: Option<String>,
    /// Y label; empty when the caller asked for none.
    pub y_label: Option<String>,
    /// Second y label; empty when the caller asked for none.
    pub y2_label: Option<String>,
    /// X scale.
    pub x_scale: ScaleKind,
    /// Y scale.
    pub y_log: bool,
    /// Second y scale.
    pub y2_log: bool,
    /// Value format.
    pub format: Option<NumFormat>,
    /// X format text; a date pattern on a time axis, a number format otherwise.
    pub x_format: Option<String>,
    /// Y format.
    pub y_format: Option<NumFormat>,
    /// Second y format.
    pub y2_format: Option<NumFormat>,
    /// Legend.
    pub legend: Legend,
    /// Theme.
    pub theme: Theme,
    /// Custom colours.
    pub colors: Option<Vec<String>>,
    /// Lines of a combo chart use the right axis.
    pub line_right: bool,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixel density.
    pub scale: f64,
    /// X lower bound.
    pub x_min: Option<f64>,
    /// X upper bound.
    pub x_max: Option<f64>,
    /// Y lower bound.
    pub y_min: Option<f64>,
    /// Y upper bound.
    pub y_max: Option<f64>,
    /// Diverging centre.
    pub center: Option<f64>,
    /// What to return.
    pub output: Output,
    /// Files to write.
    pub save: Option<SaveSpec>,
    /// Notes about fields that do not apply.
    pub notes: Vec<String>,
}

fn kind_of(name: &str) -> Res<(Kind, Option<bool>, bool)> {
    // (kind, stacked override, horizontal override)
    Ok(match name {
        "line" => (Kind::Line, None, false),
        "area" => (Kind::Area, None, false),
        "stacked_area" => (Kind::StackedArea, None, false),
        "bar" | "column" => (Kind::Bar, None, false),
        "stacked_bar" => (Kind::Bar, Some(true), false),
        "horizontal_bar" => (Kind::Bar, None, true),
        "scatter" => (Kind::Scatter, None, false),
        "bubble" => (Kind::Bubble, None, false),
        "histogram" => (Kind::Histogram, None, false),
        "box" | "boxplot" => (Kind::Box, None, false),
        "pie" => (Kind::Pie, None, false),
        "donut" => (Kind::Donut, None, false),
        "heatmap" => (Kind::Heatmap, None, false),
        "combo" => (Kind::Combo, None, false),
        other => {
            return fail(format!(
                "chart type {other:?} is not known; use line, area, stacked_area, bar, stacked_bar, horizontal_bar, scatter, bubble, histogram, box, pie, donut, heatmap or combo"
            ));
        }
    })
}

fn choice<T: Copy>(field: &str, value: Option<&str>, default: T, table: &[(&str, T)]) -> Res<T> {
    let Some(v) = value else { return Ok(default) };
    match table.iter().find(|(name, _)| *name == v) {
        Some((_, t)) => Ok(*t),
        None => fail(format!(
            "{field} {v:?} is not known; use {}",
            table.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn scale_of(field: &str, value: Option<&str>, allow_time: bool) -> Res<ScaleKind> {
    let v = choice(
        field,
        value,
        ScaleKind::Auto,
        &[
            ("auto", ScaleKind::Auto),
            ("linear", ScaleKind::Linear),
            ("log", ScaleKind::Log),
            ("time", ScaleKind::Time),
            ("category", ScaleKind::Category),
        ],
    )?;
    if !allow_time && matches!(v, ScaleKind::Time | ScaleKind::Category) {
        return fail(format!("{field} can be auto, linear or log"));
    }
    Ok(v)
}

fn clean_text(v: Option<String>) -> Option<String> {
    v.map(|s| crate::text::clean(&s))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// An axis label: `Some("")` means the caller asked for no label.
fn clean_label(v: Option<String>) -> Option<String> {
    v.map(|s| crate::text::clean(&s).trim().to_owned())
}

fn parse_format(v: Option<&str>) -> Res<Option<NumFormat>> {
    v.map(format::parse).transpose()
}

/// An x format is a number format or, on a time axis, a date pattern.
fn check_x_format(f: &str) -> Res<()> {
    match format::parse(f) {
        Ok(_) => Ok(()),
        Err(e) if !f.contains('%') => Err(e),
        Err(_) => dates::validate_pattern(f),
    }
}

/// Checks a file name for `save`: a relative path with the right extension.
pub fn save_name(name: &str, ext: &str) -> Res<String> {
    let n = name.trim();
    let last = n.rsplit('/').next().unwrap_or(n);
    let ok_ext =
        last.len() > ext.len() + 1 && last.to_ascii_lowercase().ends_with(&format!(".{ext}"));
    if !ok_ext {
        return fail(format!("save file {name:?} needs a name ending in .{ext}"));
    }
    if n.contains(['\0', '\\']) || n.starts_with('/') || n.as_bytes().get(1) == Some(&b':') {
        return fail(format!(
            "save file {name:?} must be a relative path inside the folder"
        ));
    }
    if n.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return fail(format!(
            "save file {name:?} must stay inside the folder: no empty parts, . or .."
        ));
    }
    if n.len() > 200 {
        return fail("save file names are limited to 200 characters");
    }
    Ok(n.to_owned())
}

impl Request<'_> {
    /// Validates the request and fills in the defaults.
    pub fn resolve(&self) -> Res<Spec> {
        let chart = self.chart.as_deref().ok_or("chart is required; use line, area, stacked_area, bar, scatter, bubble, histogram, box, pie, donut, heatmap or combo")?;
        let (kind, stack_override, horizontal_override) = kind_of(chart.trim())?;
        let mut notes = Vec::new();
        let width = self.width.unwrap_or(800);
        let height = self.height.unwrap_or(match kind {
            Kind::Pie | Kind::Donut => 520,
            _ => 480,
        });
        if !(MIN_WIDTH..=MAX_SIDE).contains(&width) {
            return fail(format!(
                "width must be between {MIN_WIDTH} and {MAX_SIDE} pixels"
            ));
        }
        if !(MIN_HEIGHT..=MAX_SIDE).contains(&height) {
            return fail(format!(
                "height must be between {MIN_HEIGHT} and {MAX_SIDE} pixels"
            ));
        }
        let scale = self.scale.unwrap_or(1.0);
        if !(0.5..=4.0).contains(&scale) {
            return fail("scale must be between 0.5 and 4");
        }
        let pixels =
            (f64::from(width) * scale).round() as u64 * (f64::from(height) * scale).round() as u64;
        if pixels > MAX_PIXELS {
            return fail(format!(
                "the image would be {pixels} pixels, over the limit of {MAX_PIXELS}; lower width, height or scale"
            ));
        }
        for (name, v) in [
            ("x_min", self.x_min),
            ("x_max", self.x_max),
            ("y_min", self.y_min),
            ("y_max", self.y_max),
            ("center", self.center),
        ] {
            if v.is_some_and(|v| !v.is_finite()) {
                return fail(format!("{name} must be a finite number"));
            }
        }
        if let (Some(a), Some(b)) = (self.y_min, self.y_max)
            && a >= b
        {
            return fail("y_min must be below y_max");
        }
        if let (Some(a), Some(b)) = (self.x_min, self.x_max)
            && a >= b
        {
            return fail("x_min must be below x_max");
        }
        let agg = choice(
            "agg",
            self.agg.as_deref(),
            Agg::Sum,
            &[
                ("sum", Agg::Sum),
                ("mean", Agg::Mean),
                ("count", Agg::Count),
                ("min", Agg::Min),
                ("max", Agg::Max),
                ("median", Agg::Median),
            ],
        )?;
        let sort = choice(
            "sort",
            self.sort.as_deref(),
            Sort::None,
            &[
                ("none", Sort::None),
                ("x", Sort::X),
                ("x_desc", Sort::XDesc),
                ("value", Sort::Value),
                ("value_desc", Sort::ValueDesc),
            ],
        )?;
        let stack = choice(
            "stack",
            self.stack.as_deref(),
            false,
            &[("grouped", false), ("stacked", true)],
        )?;
        if self.stack.is_some() && kind != Kind::Bar {
            notes.push("stack only applies to bar charts and was ignored".into());
        }
        if self.horizontal.is_some() && kind != Kind::Bar {
            notes.push("horizontal only applies to bar charts and was ignored".into());
        }
        let legend = choice(
            "legend",
            self.legend.as_deref(),
            Legend::Auto,
            &[
                ("auto", Legend::Auto),
                ("none", Legend::None),
                ("right", Legend::Right),
                ("bottom", Legend::Bottom),
                ("top", Legend::Top),
                ("left", Legend::Left),
            ],
        )?;
        let output = choice(
            "output",
            self.output.as_deref(),
            Output::Both,
            &[
                ("both", Output::Both),
                ("svg", Output::Svg),
                ("png", Output::Png),
            ],
        )?;
        let theme = palette::theme(self.theme.as_deref().unwrap_or("light"))?;
        let colors = self.colors.as_deref().map(palette::custom).transpose()?;
        let line_right = choice(
            "line_axis",
            self.line_axis.as_deref(),
            true,
            &[("right", true), ("left", false)],
        )?;
        let bins = match &self.bins {
            None => Bins::Auto,
            Some(BinsArg::Text(t)) if t.trim() == "auto" => Bins::Auto,
            Some(BinsArg::Text(t))
                if t.trim()
                    .parse::<usize>()
                    .is_ok_and(|n| (1..=MAX_BINS).contains(&n)) =>
            {
                Bins::Count(t.trim().parse().unwrap_or(1))
            }
            Some(BinsArg::Count(n)) if (1..=MAX_BINS as u32).contains(n) => {
                Bins::Count(*n as usize)
            }
            Some(_) => {
                return fail(format!(
                    "bins must be \"auto\" or a whole number from 1 to {MAX_BINS}"
                ));
            }
        };
        let x_scale = scale_of("x_scale", self.x_scale.as_deref(), true)?;
        let y_log = scale_of("y_scale", self.y_scale.as_deref(), false)? == ScaleKind::Log;
        let y2_log = scale_of("y2_scale", self.y2_scale.as_deref(), false)? == ScaleKind::Log;
        let save = match &self.save {
            None => None,
            Some(SaveArg::Base(base)) => {
                let b = base.trim();
                if b.is_empty() || b.contains(['\0', '\\']) || b.ends_with('/') {
                    return fail("save needs a file name like \"chart\"");
                }
                Some(SaveSpec {
                    svg: Some(save_name(&format!("{b}.svg"), "svg")?),
                    png: Some(save_name(&format!("{b}.png"), "png")?),
                })
            }
            Some(SaveArg::Files { svg, png }) => {
                if svg.is_none() && png.is_none() {
                    return fail("save needs an svg or a png file name");
                }
                Some(SaveSpec {
                    svg: svg.as_deref().map(|n| save_name(n, "svg")).transpose()?,
                    png: png.as_deref().map(|n| save_name(n, "png")).transpose()?,
                })
            }
        };
        let mut y = self.y.clone().map(Names::into_vec).unwrap_or_default();
        y.retain(|c| !c.trim().is_empty());
        let mut line = self.line.clone().map(Names::into_vec).unwrap_or_default();
        line.retain(|c| !c.trim().is_empty());
        if y.len() + line.len() > MAX_SERIES {
            return fail(format!(
                "at most {MAX_SERIES} value columns can be drawn; name fewer in y"
            ));
        }
        if !line.is_empty() && kind != Kind::Combo {
            notes.push("line only applies to combo charts and was ignored".into());
            line.clear();
        }
        if let Some(f) = &self.x_format {
            check_x_format(f)?;
        }
        Ok(Spec {
            kind,
            x: self.x.clone().filter(|s| !s.trim().is_empty()),
            y,
            line,
            series: self.series.clone().filter(|s| !s.trim().is_empty()),
            size: self.size.clone().filter(|s| !s.trim().is_empty()),
            value: self.value.clone().filter(|s| !s.trim().is_empty()),
            agg,
            agg_given: self.agg.is_some(),
            sort,
            stacked: stack_override.unwrap_or(stack) && kind == Kind::Bar,
            horizontal: (horizontal_override || self.horizontal.unwrap_or(false))
                && kind == Kind::Bar,
            bins,
            title: clean_text(self.title.clone()),
            subtitle: clean_text(self.subtitle.clone()),
            x_label: clean_label(self.x_label.clone()),
            y_label: clean_label(self.y_label.clone()),
            y2_label: clean_label(self.y2_label.clone()),
            x_scale,
            y_log,
            y2_log,
            format: parse_format(self.format.as_deref())?,
            x_format: self.x_format.clone(),
            y_format: parse_format(self.y_format.as_deref().or(self.format.as_deref()))?,
            y2_format: parse_format(self.y2_format.as_deref())?,
            legend,
            theme,
            colors,
            line_right,
            width,
            height,
            scale,
            x_min: self.x_min,
            x_max: self.x_max,
            y_min: self.y_min,
            y_max: self.y_max,
            center: self.center,
            output,
            save,
            notes,
        })
    }
}

impl Spec {
    /// The columns the chart reads, when the request names them all, so a
    /// reader can skip the rest. `None` when a default must look at the data.
    pub fn needed_columns(&self) -> Option<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |c: &Option<String>| {
            if let Some(c) = c {
                out.push(c.trim().to_owned());
            }
        };
        let complete = match self.kind {
            Kind::Histogram => self.x.is_some(),
            Kind::Box => !self.y.is_empty(),
            Kind::Heatmap => self.x.is_some() && self.y.len() == 1 && self.value.is_some(),
            Kind::Pie | Kind::Donut => self.x.is_some() && !self.y.is_empty(),
            _ => self.x.is_some() && !(self.y.is_empty() && self.line.is_empty()),
        };
        if !complete {
            return None;
        }
        add(&self.x);
        add(&self.series);
        add(&self.size);
        add(&self.value);
        out.extend(
            self.y
                .iter()
                .chain(self.line.iter())
                .map(|c| c.trim().to_owned()),
        );
        out.sort();
        out.dedup();
        Some(out)
    }
}

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
