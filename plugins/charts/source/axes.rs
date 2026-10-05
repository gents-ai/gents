//! Builds axes from data ranges: round numeric ticks, logarithmic decades,
//! calendar ticks and category slots.

use crate::dates;
use crate::err::{Res, fail};
use crate::format::{NumFormat, axis_format, tick_labels};
use crate::frame::{Axis, AxisKind, Tick};
use crate::scale::{Ticks, log_ticks, nice_ticks};

/// The domain of a numeric axis and how to tick it.
#[derive(Debug, Clone, Copy)]
pub struct NumSpec {
    /// Data minimum.
    pub min: f64,
    /// Data maximum.
    pub max: f64,
    /// Caller bound replacing the minimum.
    pub user_min: Option<f64>,
    /// Caller bound replacing the maximum.
    pub user_max: Option<f64>,
    /// Include zero in the domain.
    pub zero: bool,
    /// Extend the domain to round numbers.
    pub nice: bool,
    /// Logarithmic scale.
    pub log: bool,
    /// Ticks only on whole numbers (counts).
    pub integer: bool,
}

fn within(v: f64, lo: f64, hi: f64) -> bool {
    let eps = (hi - lo).abs() * 1e-9;
    v >= lo - eps && v <= hi + eps
}

/// A numeric axis for `spec` with about `target` ticks.
pub fn numeric(
    spec: NumSpec,
    target: usize,
    fmt: Option<&NumFormat>,
    label: Option<String>,
) -> Axis {
    let (mut lo, mut hi) = (spec.min, spec.max);
    if !lo.is_finite() || !hi.is_finite() {
        (lo, hi) = (0.0, 1.0);
    }
    if spec.zero && !spec.log {
        lo = lo.min(0.0);
        hi = hi.max(0.0);
    }
    let user = spec.user_min.is_some() || spec.user_max.is_some();
    if let Some(v) = spec.user_min {
        lo = v;
    }
    if let Some(v) = spec.user_max {
        hi = v;
    }
    if lo > hi {
        std::mem::swap(&mut lo, &mut hi);
    }
    if spec.log {
        let t = log_ticks(lo.max(f64::MIN_POSITIVE), hi.max(lo.max(f64::MIN_POSITIVE)));
        let (d0, d1) = if user { (lo, hi) } else { (t.lo, t.hi) };
        let values: Vec<f64> = t
            .values
            .into_iter()
            .filter(|v| within(*v, d0, d1))
            .collect();
        let f = axis_format(fmt, &values, 0);
        let labels = tick_labels(&f, &values, 0);
        let ticks = values
            .into_iter()
            .zip(labels)
            .map(|(value, label)| Tick { value, label })
            .collect();
        return Axis {
            kind: AxisKind::Cont {
                log: true,
                d0,
                d1,
                ticks,
            },
            label,
        };
    }
    if lo == hi {
        let pad = if lo == 0.0 { 1.0 } else { lo.abs() * 0.1 };
        lo -= pad;
        hi += pad;
    }
    let mut t = nice_ticks(lo, hi, target);
    if spec.integer && t.step < 1.0 {
        let (first, last) = (lo.floor() as i64, hi.ceil() as i64);
        t = Ticks {
            values: (first..=last).map(|v| v as f64).collect(),
            step: 1.0,
            decimals: 0,
        };
    }
    let (d0, d1) = if user || !spec.nice {
        (lo, hi)
    } else {
        (t.values[0], t.values[t.values.len() - 1])
    };
    let values: Vec<f64> = t
        .values
        .iter()
        .copied()
        .filter(|v| within(*v, d0, d1))
        .collect();
    let f = axis_format(fmt, &values, t.decimals);
    let labels = tick_labels(&f, &values, t.decimals);
    let ticks = values
        .into_iter()
        .zip(labels)
        .map(|(value, label)| Tick { value, label })
        .collect();
    Axis {
        kind: AxisKind::Cont {
            log: false,
            d0,
            d1,
            ticks,
        },
        label,
    }
}

/// A calendar axis over `[min, max]` epoch seconds.
pub fn time(
    min: f64,
    max: f64,
    user_min: Option<f64>,
    user_max: Option<f64>,
    target: usize,
    pattern: Option<&str>,
    label: Option<String>,
) -> Axis {
    let mut lo = user_min.unwrap_or(min);
    let mut hi = user_max.unwrap_or(max);
    if lo >= hi {
        lo -= 43_200.0;
        hi = lo + 86_400.0;
    }
    let t = dates::ticks(lo, hi, target, pattern);
    let ticks = t
        .values
        .iter()
        .zip(&t.labels)
        .filter(|(v, _)| within(**v, lo, hi))
        .map(|(v, l)| Tick {
            value: *v,
            label: l.clone(),
        })
        .collect();
    Axis {
        kind: AxisKind::Cont {
            log: false,
            d0: lo,
            d1: hi,
            ticks,
        },
        label,
    }
}

/// A category axis.
pub fn band(labels: Vec<String>, label: Option<String>) -> Axis {
    Axis {
        kind: AxisKind::Band { labels },
        label,
    }
}

/// Refuses non-positive values on a logarithmic axis, naming the column and
/// the first offender.
pub fn require_positive(
    axis: &str,
    column: &str,
    values: impl IntoIterator<Item = f64>,
) -> Res<()> {
    let mut bad = 0usize;
    let mut first = None;
    for (i, v) in values.into_iter().enumerate() {
        if v.is_finite() && v <= 0.0 {
            bad += 1;
            first.get_or_insert((i + 1, v));
        }
    }
    match first {
        None => Ok(()),
        Some((row, v)) => fail(format!(
            "the {axis} axis is logarithmic but column {column:?} has {bad} values at or below zero (first is {} in row {row}); remove them or use a linear axis",
            crate::format::compact(v)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(min: f64, max: f64) -> NumSpec {
        NumSpec {
            min,
            max,
            user_min: None,
            user_max: None,
            zero: false,
            nice: true,
            log: false,
            integer: false,
        }
    }

    fn cont(a: &Axis) -> (f64, f64, Vec<(f64, String)>) {
        match &a.kind {
            AxisKind::Cont { d0, d1, ticks, .. } => (
                *d0,
                *d1,
                ticks.iter().map(|t| (t.value, t.label.clone())).collect(),
            ),
            AxisKind::Band { .. } => panic!("band"),
        }
    }

    #[test]
    fn a_nice_axis_extends_the_domain_to_its_end_ticks() {
        let (d0, d1, t) = cont(&numeric(spec(3.2, 97.1), 5, None, None));
        assert_eq!((d0, d1), (0.0, 100.0));
        assert_eq!(t.len(), 6);
        assert_eq!(t[0], (0.0, "0".into()));
        assert_eq!(t[5], (100.0, "100".into()));
    }

    #[test]
    fn without_nice_the_domain_is_the_data_and_ticks_stay_inside() {
        let mut s = spec(3.2, 97.1);
        s.nice = false;
        let (d0, d1, t) = cont(&numeric(s, 5, None, None));
        assert_eq!((d0, d1), (3.2, 97.1));
        assert!(t.iter().all(|(v, _)| *v >= 3.2 && *v <= 97.1));
        assert_eq!(t.first().unwrap().0, 20.0);
    }

    #[test]
    fn zero_is_included_on_request_for_all_positive_and_all_negative_data() {
        let mut s = spec(40.0, 90.0);
        s.zero = true;
        assert_eq!(cont(&numeric(s, 5, None, None)).0, 0.0);
        let mut n = spec(-90.0, -40.0);
        n.zero = true;
        assert_eq!(cont(&numeric(n, 5, None, None)).1, 0.0);
    }

    #[test]
    fn caller_bounds_fix_the_domain_exactly() {
        let mut s = spec(0.0, 500.0);
        s.user_min = Some(10.0);
        s.user_max = Some(90.0);
        let (d0, d1, t) = cont(&numeric(s, 5, None, None));
        assert_eq!((d0, d1), (10.0, 90.0));
        assert!(t.iter().all(|(v, _)| *v >= 10.0 && *v <= 90.0));
        let mut one = spec(0.0, 500.0);
        one.user_max = Some(100.0);
        assert_eq!(cont(&numeric(one, 5, None, None)).1, 100.0);
    }

    #[test]
    fn a_degenerate_range_is_widened_around_the_value() {
        let (d0, d1, t) = cont(&numeric(spec(5.0, 5.0), 5, None, None));
        assert!(d0 < 5.0 && d1 > 5.0 && t.len() >= 2);
        let (d0, d1, _) = cont(&numeric(spec(0.0, 0.0), 5, None, None));
        assert!(d0 < 0.0 && d1 > 0.0);
    }

    #[test]
    fn labels_follow_the_automatic_format_and_a_custom_one() {
        let (_, _, t) = cont(&numeric(spec(0.0, 1.0), 5, None, None));
        assert_eq!(
            t.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
            ["0.0", "0.2", "0.4", "0.6", "0.8", "1.0"]
        );
        let f = crate::format::parse("{.0%}").unwrap();
        let (_, _, t) = cont(&numeric(spec(0.0, 1.0), 5, Some(&f), None));
        assert_eq!(
            t.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
            ["0%", "20%", "40%", "60%", "80%", "100%"]
        );
        let (_, _, t) = cont(&numeric(spec(0.0, 3e6), 3, None, None));
        assert!(t.iter().any(|x| x.1 == "2M"), "{t:?}");
    }

    #[test]
    fn a_log_axis_spans_whole_decades_and_uses_one_si_style_for_every_label() {
        let mut s = spec(3.0, 4.0e5);
        s.log = true;
        let a = numeric(s, 5, None, Some("n".into()));
        let (d0, d1, t) = cont(&a);
        assert_eq!((d0, d1), (1.0, 1e6));
        assert_eq!(
            t.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
            ["1", "10", "100", "1k", "10k", "100k", "1M"]
        );
        assert!(matches!(a.kind, AxisKind::Cont { log: true, .. }));
        assert_eq!(a.label.as_deref(), Some("n"));
    }

    #[test]
    fn a_log_axis_with_caller_bounds_keeps_them() {
        let mut s = spec(1.0, 1000.0);
        s.log = true;
        s.user_min = Some(5.0);
        let (d0, _, t) = cont(&numeric(s, 5, None, None));
        assert_eq!(d0, 5.0);
        assert!(t.iter().all(|(v, _)| *v >= 5.0));
    }

    #[test]
    fn a_time_axis_uses_the_data_range_and_calendar_ticks() {
        let a = dates::parse("2024-01-15").unwrap();
        let b = dates::parse("2024-12-20").unwrap();
        let (d0, d1, t) = cont(&time(a, b, None, None, 6, None, None));
        assert_eq!((d0, d1), (a, b));
        assert!(t.len() >= 3 && t.iter().all(|(v, _)| *v >= a && *v <= b));
        assert!(t[0].1.contains("2024"));
    }

    #[test]
    fn a_single_instant_gets_a_one_day_axis_around_it() {
        let a = dates::parse("2024-03-01").unwrap();
        let (d0, d1, t) = cont(&time(a, a, None, None, 5, None, None));
        assert!(d0 < a && d1 > a && d1 - d0 == 86_400.0);
        assert!(!t.is_empty());
    }

    #[test]
    fn a_time_axis_takes_a_date_pattern() {
        let a = dates::parse("2020-01-01").unwrap();
        let b = dates::parse("2024-06-01").unwrap();
        let (_, _, t) = cont(&time(a, b, None, None, 5, Some("'%y"), None));
        assert!(t.iter().all(|x| x.1.starts_with('\'')));
    }

    #[test]
    fn a_band_axis_keeps_its_labels() {
        let a = band(vec!["a".into(), "b".into()], Some("k".into()));
        assert_eq!(
            a.kind,
            AxisKind::Band {
                labels: vec!["a".into(), "b".into()]
            }
        );
    }

    #[test]
    fn a_log_axis_refuses_non_positive_values_and_names_the_first() {
        assert!(require_positive("y", "v", [1.0, 2.0, f64::NAN]).is_ok());
        let e = require_positive("y", "v", [3.0, 0.0, -4.0]).unwrap_err().0;
        assert!(
            e.contains("column \"v\" has 2 values at or below zero")
                && e.contains("first is 0 in row 2"),
            "{e}"
        );
        assert!(e.contains("linear axis"));
    }
}
