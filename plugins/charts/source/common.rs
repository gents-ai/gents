//! Pieces shared by the chart types: the result of drawing, axis labels with
//! sensible defaults, x axes for numbers, dates and categories, and the
//! wording of descriptions.

use serde_json::{Map, Value};

use crate::axes::{self, NumSpec};
use crate::dates;
use crate::err::Res;
use crate::format::{self, NumFormat, compact};
use crate::frame::Axis;
use crate::shape::{XData, XKind};
use crate::spec::Spec;
use crate::svg::Svg;

/// What one series came to.
#[derive(Debug, Clone, Default)]
pub struct SeriesInfo {
    /// Name.
    pub name: String,
    /// Mark: line, area, bar, point, slice, box, cell, bin.
    pub mark: &'static str,
    /// Colour.
    pub color: String,
    /// `left` or `right`.
    pub axis: &'static str,
    /// Numbers in the series.
    pub points: usize,
    /// Marks actually drawn when the series was reduced.
    pub drawn: usize,
    /// Rows without a number.
    pub gaps: usize,
    /// Lowest value.
    pub min: Option<f64>,
    /// Highest value.
    pub max: Option<f64>,
    /// Extra facts, such as quartiles.
    pub extra: Map<String, Value>,
}

/// A drawn chart.
pub struct Built {
    /// The picture.
    pub svg: Svg,
    /// The series.
    pub series: Vec<SeriesInfo>,
    /// The description of the chart.
    pub alt: String,
    /// The rectangle the data is drawn in.
    pub plot: crate::frame::Rect,
}

/// An axis label: the caller's, else `default`; an empty one means none.
pub fn label_or(given: &Option<String>, default: &str) -> Option<String> {
    match given {
        Some(s) if s.is_empty() => None,
        Some(s) => Some(s.clone()),
        None if default.is_empty() => None,
        None => Some(default.to_owned()),
    }
}

/// True when every value is a whole number from 1000 to 2999: years, which
/// are written without a thousands separator.
pub fn years_like(values: &[f64]) -> bool {
    let mut any = false;
    for v in values.iter().filter(|v| !v.is_nan()) {
        if v.fract() != 0.0 || !(1000.0..=2999.0).contains(v) {
            return false;
        }
        any = true;
    }
    any
}

/// Parses an x format as a number format.
pub fn number_format(text: Option<&str>) -> Option<NumFormat> {
    text.and_then(|t| format::parse(t).ok())
}

/// The finite range of `values`, or `None`.
pub fn range(values: impl IntoIterator<Item = f64>) -> Option<(f64, f64)> {
    values
        .into_iter()
        .filter(|v| v.is_finite())
        .fold(None, |a, v| match a {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
}

/// An x axis for `x` over `extent` (the data range of numbers or dates).
pub fn x_axis(spec: &Spec, x: &XData, extent: (f64, f64), target: usize, nice: bool) -> Axis {
    let label = label_or(&spec.x_label, &x.name);
    match x.kind {
        XKind::Cat => axes::band(x.cats.clone(), label),
        XKind::Time => axes::time(
            extent.0,
            extent.1,
            spec.x_min,
            spec.x_max,
            target,
            spec.x_format.as_deref().filter(|f| f.contains('%')),
            label,
        ),
        XKind::Num => {
            let mut fmt = number_format(spec.x_format.as_deref());
            if fmt.is_none() && x.years {
                let mut f = NumFormat::auto();
                f.group = false;
                f.digits = Some(0);
                fmt = Some(f);
            }
            axes::numeric(
                NumSpec {
                    min: extent.0,
                    max: extent.1,
                    user_min: spec.x_min,
                    user_max: spec.x_max,
                    zero: false,
                    nice,
                    log: spec.x_scale == crate::spec::ScaleKind::Log,
                    integer: false,
                },
                target,
                fmt.as_ref(),
                label,
            )
        }
    }
}

/// Widens `(lo, hi)` by `pad` of its span on each side that is not a zero
/// baseline, so marks do not sit on the frame.
pub fn padded(extent: (f64, f64), pad: f64, zero: bool) -> (f64, f64) {
    let (lo, hi) = extent;
    let span = if hi > lo { hi - lo } else { lo.abs().max(1.0) };
    let mut lo_p = if zero && lo >= 0.0 {
        lo
    } else {
        lo - pad * span
    };
    let mut hi_p = if zero && hi <= 0.0 {
        hi
    } else {
        hi + pad * span
    };
    // Data on one side of zero never grows an axis across it.
    if lo >= 0.0 {
        lo_p = lo_p.max(0.0);
    }
    if hi <= 0.0 {
        hi_p = hi_p.min(0.0);
    }
    (lo_p, hi_p)
}

/// How a y axis is built from its data range.
#[derive(Debug, Clone, Copy)]
pub struct YRange {
    /// Data range.
    pub extent: (f64, f64),
    /// Include zero.
    pub zero: bool,
    /// Logarithmic.
    pub log: bool,
    /// Headroom as a fraction of the span.
    pub pad: f64,
    /// Ticks only on whole numbers (counts).
    pub integer: bool,
}

/// A y axis over `range` with about `target` ticks.
pub fn y_axis(
    spec: &Spec,
    range: YRange,
    target: usize,
    fmt: Option<&NumFormat>,
    label: Option<String>,
) -> Axis {
    let extent = if range.log {
        range.extent
    } else {
        padded(range.extent, range.pad, range.zero)
    };
    let num = NumSpec {
        min: extent.0,
        max: extent.1,
        user_min: spec.y_min,
        user_max: spec.y_max,
        zero: range.zero,
        nice: true,
        log: range.log,
        integer: range.integer,
    };
    axes::numeric(num, target, fmt, label)
}

/// Text of an x value.
pub fn x_text(x: &XData, v: f64) -> String {
    match x.kind {
        XKind::Cat => x.cats.get(v as usize).cloned().unwrap_or_default(),
        XKind::Time => time_text(v),
        XKind::Num => {
            if x.years {
                crate::cols::number_label(v)
            } else {
                compact(v)
            }
        }
    }
}

/// A date, with the time when it is not midnight.
pub fn time_text(t: f64) -> String {
    if t.rem_euclid(86_400.0) == 0.0 {
        dates::format(t, "%Y-%m-%d")
    } else {
        dates::format(t, "%Y-%m-%d %H:%M")
    }
}

/// "1 outlier", "2 outliers", "0 outliers".
pub fn count_of(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Opening sentence of a description.
pub fn intro(kind: &str, title: Option<&str>) -> String {
    match title {
        Some(t) => format!("{kind} titled {t:?}."),
        None => format!("{kind}."),
    }
}

/// "x axis: month, Jan to Mar." style sentence.
pub fn axis_sentence(name: &str, label: Option<&str>, lo: &str, hi: &str) -> String {
    match label {
        Some(l) => format!("{name} axis: {l}, from {lo} to {hi}."),
        None => format!("{name} axis from {lo} to {hi}."),
    }
}

/// The lowest and highest y of `pts` with their x.
pub fn extremes(pts: &[(f64, f64)]) -> Option<((f64, f64), (f64, f64))> {
    let mut lo: Option<(f64, f64)> = None;
    let mut hi: Option<(f64, f64)> = None;
    for p in pts.iter().filter(|p| p.1.is_finite()) {
        if lo.is_none_or(|l| p.1 < l.1) {
            lo = Some(*p);
        }
        if hi.is_none_or(|h| p.1 > h.1) {
            hi = Some(*p);
        }
    }
    lo.zip(hi)
}

/// Fails with the log axis message unless every positive-only value is positive.
pub fn check_log(axis: &str, column: &str, values: &[f64]) -> Res<()> {
    axes::require_positive(axis, column, values.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_default_to_the_column_and_an_empty_label_means_none() {
        assert_eq!(label_or(&None, "month"), Some("month".into()));
        assert_eq!(label_or(&None, ""), None);
        assert_eq!(
            label_or(&Some("Sales".into()), "month"),
            Some("Sales".into())
        );
        assert_eq!(label_or(&Some(String::new()), "month"), None);
    }

    #[test]
    fn years_are_whole_numbers_between_1000_and_2999() {
        assert!(years_like(&[2019.0, 2020.0, f64::NAN]));
        assert!(!years_like(&[2019.5]));
        assert!(!years_like(&[999.0, 2000.0]));
        assert!(!years_like(&[3000.0]));
        assert!(!years_like(&[]));
    }

    #[test]
    fn ranges_skip_non_finite_values() {
        assert_eq!(
            range([3.0, f64::NAN, -1.0, f64::INFINITY, 7.0]),
            Some((-1.0, 7.0))
        );
        assert_eq!(range([f64::NAN]), None);
    }

    #[test]
    fn padding_leaves_a_zero_baseline_alone_and_widens_the_other_side() {
        assert_eq!(padded((0.0, 100.0), 0.05, true), (0.0, 105.0));
        assert_eq!(padded((10.0, 20.0), 0.1, false), (9.0, 21.0));
        assert_eq!(padded((-50.0, 100.0), 0.1, true), (-65.0, 115.0));
        assert_eq!(padded((-40.0, -10.0), 0.1, true), (-43.0, -10.0));
        assert_eq!(
            padded((1.0, 100.0), 0.1, false),
            (0.0, 109.9),
            "all-positive data does not grow negative"
        );
        assert_eq!(padded((-100.0, -1.0), 0.1, false), (-109.9, 0.0));
        let (lo, hi) = padded((5.0, 5.0), 0.1, false);
        assert!(lo < 5.0 && hi > 5.0);
    }

    #[test]
    fn x_text_reads_categories_dates_and_numbers() {
        let cat = XData {
            kind: XKind::Cat,
            name: "k".into(),
            v: vec![],
            cats: vec!["a".into(), "b".into()],
            years: false,
        };
        assert_eq!(x_text(&cat, 1.0), "b");
        assert_eq!(x_text(&cat, 9.0), "");
        let time = XData {
            kind: XKind::Time,
            name: "d".into(),
            v: vec![],
            cats: vec![],
            years: false,
        };
        assert_eq!(x_text(&time, 1_704_067_200.0), "2024-01-01");
        assert_eq!(
            x_text(&time, 1_704_067_200.0 + 3600.0 * 5.0),
            "2024-01-01 05:00"
        );
        let num = XData {
            kind: XKind::Num,
            name: "n".into(),
            v: vec![],
            cats: vec![],
            years: false,
        };
        assert_eq!(x_text(&num, 2020.0), "2,020");
        assert_eq!(x_text(&num, 12.5), "12.5");
        assert_eq!(x_text(&num, 1234567.0), "1,234,567");
        let years = XData { years: true, ..num };
        assert_eq!(x_text(&years, 2020.0), "2020");
    }

    #[test]
    fn counts_agree_with_their_noun() {
        assert_eq!(count_of(0, "outlier"), "0 outliers");
        assert_eq!(count_of(1, "outlier"), "1 outlier");
        assert_eq!(count_of(2, "outlier"), "2 outliers");
    }

    #[test]
    fn description_sentences_are_plain() {
        assert_eq!(
            intro("Line chart", Some("Sales")),
            "Line chart titled \"Sales\"."
        );
        assert_eq!(intro("Line chart", None), "Line chart.");
        assert_eq!(
            axis_sentence("X", Some("month"), "Jan", "Mar"),
            "X axis: month, from Jan to Mar."
        );
        assert_eq!(axis_sentence("Y", None, "0", "9"), "Y axis from 0 to 9.");
    }

    #[test]
    fn extremes_find_the_first_lowest_and_highest() {
        let p = [
            (0.0, 5.0),
            (1.0, 2.0),
            (2.0, 9.0),
            (3.0, 2.0),
            (4.0, f64::NAN),
        ];
        assert_eq!(extremes(&p), Some(((1.0, 2.0), (2.0, 9.0))));
        assert_eq!(extremes(&[(0.0, f64::NAN)]), None);
    }

    #[test]
    fn log_checks_name_the_axis_and_column() {
        assert!(check_log("y", "v", &[1.0, 2.0]).is_ok());
        assert!(
            check_log("x", "w", &[1.0, 0.0])
                .unwrap_err()
                .0
                .starts_with("the x axis is logarithmic but column \"w\"")
        );
    }
}
