//! The arithmetic behind the chart types: quantiles and box statistics,
//! histogram bins, stacking and pie angles. Plain functions over numbers, so
//! each can be checked against values worked out by hand.

use crate::scale::nice_ticks;
use crate::spec::{Bins, MAX_BINS};

/// Quantile `q` (0 to 1) of ascending `sorted` by linear interpolation
/// between closest ranks (the method most statistics packages call type 7).
pub fn quantile(sorted: &[f64], q: f64) -> f64 {
    match sorted.len() {
        0 => f64::NAN,
        1 => sorted[0],
        n => {
            let h = (n - 1) as f64 * q.clamp(0.0, 1.0);
            let lo = h.floor() as usize;
            let hi = (lo + 1).min(n - 1);
            sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
        }
    }
}

/// Box plot statistics with Tukey whiskers.
#[derive(Debug, Clone, PartialEq)]
pub struct BoxStats {
    /// Values counted.
    pub n: usize,
    /// Smallest value.
    pub min: f64,
    /// First quartile.
    pub q1: f64,
    /// Median.
    pub median: f64,
    /// Third quartile.
    pub q3: f64,
    /// Largest value.
    pub max: f64,
    /// Lowest value within 1.5 interquartile ranges below the first quartile.
    pub whisker_lo: f64,
    /// Highest value within 1.5 interquartile ranges above the third quartile.
    pub whisker_hi: f64,
    /// Values beyond the whiskers, ascending.
    pub outliers: Vec<f64>,
}

/// Statistics of `values` (finite numbers); `None` when there are none.
pub fn box_stats(values: &[f64]) -> Option<BoxStats> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let (q1, median, q3) = (quantile(&v, 0.25), quantile(&v, 0.5), quantile(&v, 0.75));
    let iqr = q3 - q1;
    let (lo_fence, hi_fence) = (q1 - 1.5 * iqr, q3 + 1.5 * iqr);
    let whisker_lo = v.iter().copied().find(|x| *x >= lo_fence).unwrap_or(q1);
    let whisker_hi = v
        .iter()
        .rev()
        .copied()
        .find(|x| *x <= hi_fence)
        .unwrap_or(q3);
    let outliers = v
        .iter()
        .copied()
        .filter(|x| *x < whisker_lo || *x > whisker_hi)
        .collect();
    Some(BoxStats {
        n: v.len(),
        min: v[0],
        q1,
        median,
        q3,
        max: v[v.len() - 1],
        whisker_lo,
        whisker_hi,
        outliers,
    })
}

/// Bin edges for `n` values spanning `[min, max]`. An exact count gives that
/// many equal bins; `auto` gives round edges, about as many bins as Sturges'
/// rule suggests (between 5 and 40).
pub fn bin_edges(min: f64, max: f64, n: usize, bins: Bins) -> Vec<f64> {
    let (lo, hi) = if min == max {
        (min - 0.5, max + 0.5)
    } else {
        (min, max)
    };
    match bins {
        Bins::Count(k) => {
            let k = k.clamp(1, MAX_BINS);
            (0..=k)
                .map(|i| {
                    if i == k {
                        hi
                    } else {
                        lo + (hi - lo) * i as f64 / k as f64
                    }
                })
                .collect()
        }
        Bins::Auto => {
            let sturges = (usize::BITS - n.saturating_sub(1).leading_zeros()) as usize + 1;
            let ticks = nice_ticks(lo, hi, sturges.clamp(5, 40));
            let mut edges = ticks.values;
            if edges.len() < 2 {
                edges = vec![lo, hi];
            }
            if edges.len() > MAX_BINS + 1 {
                edges.truncate(MAX_BINS + 1);
            }
            edges
        }
    }
}

/// Counts of `values` per bin of `edges`: a value belongs to the bin whose
/// left edge is at or below it and right edge above it; the last bin also
/// takes values equal to its right edge. Values outside the edges are not
/// counted.
pub fn histogram(values: &[f64], edges: &[f64]) -> Vec<u64> {
    let bins = edges.len().saturating_sub(1);
    let mut counts = vec![0u64; bins];
    if bins == 0 {
        return counts;
    }
    for v in values {
        if v.is_nan() || *v < edges[0] || *v > edges[bins] {
            continue;
        }
        let i = edges
            .partition_point(|e| *e <= *v)
            .saturating_sub(1)
            .min(bins - 1);
        counts[i] += 1;
    }
    counts
}

/// A stacked layer: the bottom and top of the layer at every position.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// Lower edge per position.
    pub lo: Vec<f64>,
    /// Upper edge per position.
    pub hi: Vec<f64>,
}

/// Stacks `series` (aligned by position, `NaN` counts as nothing) on top of
/// each other from zero, upward for positive values and downward for negative
/// ones, so every layer starts where the previous one on its side ended.
pub fn stack(series: &[Vec<f64>]) -> Vec<Layer> {
    let n = series.first().map_or(0, Vec::len);
    let mut pos = vec![0.0; n];
    let mut neg = vec![0.0; n];
    series
        .iter()
        .map(|s| {
            let mut layer = Layer {
                lo: vec![0.0; n],
                hi: vec![0.0; n],
            };
            for (i, v) in s.iter().enumerate() {
                let v = if v.is_nan() { 0.0 } else { *v };
                let base = if v >= 0.0 { &mut pos[i] } else { &mut neg[i] };
                layer.lo[i] = *base;
                *base += v;
                layer.hi[i] = *base;
            }
            layer
        })
        .collect()
}

/// Start and end angle in degrees of each slice, clockwise from the top, for
/// non-negative `values`. The last slice ends at exactly 360 so the slices
/// always close the circle.
pub fn pie_angles(values: &[f64]) -> Vec<(f64, f64)> {
    let total: f64 = values.iter().sum();
    if total <= 0.0 {
        return vec![(0.0, 0.0); values.len()];
    }
    let mut run = 0.0;
    let last = values.len().saturating_sub(1);
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let start = run / total * 360.0;
            run += v;
            let end = if i == last {
                360.0
            } else {
                run / total * 360.0
            };
            (start, end)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_match_values_worked_out_by_hand() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(quantile(&v, 0.0), 1.0);
        assert_eq!(quantile(&v, 0.25), 2.0);
        assert_eq!(quantile(&v, 0.5), 3.0);
        assert_eq!(quantile(&v, 0.75), 4.0);
        assert_eq!(quantile(&v, 1.0), 5.0);
        let w = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(quantile(&w, 0.5), 2.5);
        assert_eq!(quantile(&w, 0.25), 1.75);
        assert_eq!(quantile(&w, 0.75), 3.25);
        assert_eq!(quantile(&[7.0], 0.9), 7.0);
        assert!(quantile(&[], 0.5).is_nan());
        assert_eq!(quantile(&v, 5.0), 5.0);
        assert_eq!(quantile(&v, -1.0), 1.0);
    }

    #[test]
    fn box_statistics_for_a_textbook_sample() {
        // 1..=9: q1 = 3, median = 5, q3 = 7, iqr = 4, fences -3 and 13.
        let b = box_stats(&[9.0, 1.0, 5.0, 3.0, 7.0, 2.0, 8.0, 4.0, 6.0]).unwrap();
        assert_eq!(
            (b.n, b.min, b.q1, b.median, b.q3, b.max),
            (9, 1.0, 3.0, 5.0, 7.0, 9.0)
        );
        assert_eq!((b.whisker_lo, b.whisker_hi), (1.0, 9.0));
        assert!(b.outliers.is_empty());
    }

    #[test]
    fn values_beyond_one_and_a_half_iqr_are_outliers_and_whiskers_stop_at_data() {
        // q1 = 3, q3 = 7 for 1..=9 plus 30: recompute with 10 values.
        let mut v: Vec<f64> = (1..=9).map(f64::from).collect();
        v.push(30.0);
        v.push(-20.0);
        let b = box_stats(&v).unwrap();
        assert_eq!(b.outliers, [-20.0, 30.0]);
        assert_eq!((b.whisker_lo, b.whisker_hi), (1.0, 9.0));
        assert_eq!((b.min, b.max), (-20.0, 30.0));
    }

    #[test]
    fn the_whisker_is_the_extreme_value_inside_the_fence_not_the_fence() {
        let b = box_stats(&[10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 40.0]).unwrap();
        // q1 = 11.75, q3 = 15.25, iqr = 3.5, upper fence 20.5.
        assert_eq!(b.whisker_hi, 16.0);
        assert_eq!(b.outliers, [40.0]);
    }

    #[test]
    fn box_statistics_of_one_value_and_of_equal_values() {
        let b = box_stats(&[4.0]).unwrap();
        assert_eq!(
            (b.q1, b.median, b.q3, b.whisker_lo, b.whisker_hi),
            (4.0, 4.0, 4.0, 4.0, 4.0)
        );
        let b = box_stats(&[2.0; 6]).unwrap();
        assert!(b.outliers.is_empty() && b.whisker_lo == 2.0 && b.whisker_hi == 2.0);
        assert_eq!(box_stats(&[]), None);
    }

    #[test]
    fn an_even_sample_interpolates_its_quartiles() {
        let b = box_stats(&[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!((b.q1, b.median, b.q3), (1.75, 2.5, 3.25));
    }

    #[test]
    fn exact_bin_counts_split_the_range_evenly() {
        let e = bin_edges(0.0, 10.0, 100, Bins::Count(5));
        assert_eq!(e, [0.0, 2.0, 4.0, 6.0, 8.0, 10.0]);
        let e = bin_edges(3.0, 3.0, 5, Bins::Count(2));
        assert_eq!(e, [2.5, 3.0, 3.5]);
        assert_eq!(bin_edges(0.0, 1.0, 5, Bins::Count(1)), [0.0, 1.0]);
    }

    #[test]
    fn auto_bins_use_round_edges_that_cover_the_data() {
        let e = bin_edges(3.2, 97.1, 100, Bins::Auto);
        assert!(e[0] <= 3.2 && *e.last().unwrap() >= 97.1);
        let step = e[1] - e[0];
        assert!(e.windows(2).all(|w| ((w[1] - w[0]) - step).abs() < 1e-9));
        assert!((5..=20).contains(&(e.len() - 1)), "{} bins", e.len() - 1);
        let tiny = bin_edges(5.0, 5.0, 1, Bins::Auto);
        assert!(tiny.len() >= 2 && tiny[0] < 5.0 && *tiny.last().unwrap() > 5.0);
    }

    #[test]
    fn the_number_of_auto_bins_grows_with_the_sample_but_stays_readable() {
        let few = bin_edges(0.0, 100.0, 10, Bins::Auto).len();
        let many = bin_edges(0.0, 100.0, 1_000_000, Bins::Auto).len();
        assert!(few <= many);
        assert!(many <= 100, "{many}");
    }

    #[test]
    fn counts_follow_the_half_open_rule_with_the_last_bin_closed() {
        let edges = [0.0, 10.0, 20.0, 30.0];
        let counts = histogram(&[0.0, 9.99, 10.0, 19.0, 20.0, 29.0, 30.0], &edges);
        assert_eq!(counts, [2, 2, 3]);
    }

    #[test]
    fn values_outside_the_edges_are_not_counted_and_nothing_is_double_counted() {
        let edges = [0.0, 1.0, 2.0];
        assert_eq!(histogram(&[-0.1, 2.1, f64::NAN, 0.5, 1.5], &edges), [1, 1]);
        assert_eq!(histogram(&[1.0, 2.0], &[5.0]), Vec::<u64>::new());
        assert_eq!(histogram(&[], &edges), [0, 0]);
    }

    #[test]
    fn stacking_adds_layers_from_zero_and_the_top_is_the_column_sum() {
        let s = [
            vec![1.0, 2.0, 3.0],
            vec![4.0, 0.0, 1.0],
            vec![2.0, 2.0, 2.0],
        ];
        let l = stack(&s);
        assert_eq!(l[0].lo, [0.0; 3]);
        assert_eq!(l[0].hi, [1.0, 2.0, 3.0]);
        assert_eq!(l[1].lo, [1.0, 2.0, 3.0]);
        assert_eq!(l[1].hi, [5.0, 2.0, 4.0]);
        assert_eq!(l[2].hi, [7.0, 4.0, 6.0]);
    }

    #[test]
    fn negative_values_stack_downward_separately_and_gaps_add_nothing() {
        let s = [vec![3.0, -2.0], vec![-1.0, -3.0], vec![f64::NAN, 4.0]];
        let l = stack(&s);
        assert_eq!((l[0].lo[0], l[0].hi[0]), (0.0, 3.0));
        assert_eq!((l[1].lo[0], l[1].hi[0]), (0.0, -1.0));
        assert_eq!((l[2].lo[0], l[2].hi[0]), (3.0, 3.0));
        assert_eq!((l[0].lo[1], l[0].hi[1]), (0.0, -2.0));
        assert_eq!((l[1].lo[1], l[1].hi[1]), (-2.0, -5.0));
        assert_eq!((l[2].lo[1], l[2].hi[1]), (0.0, 4.0));
        assert!(stack(&[]).is_empty());
    }

    #[test]
    fn pie_angles_follow_the_shares_and_close_the_circle() {
        let a = pie_angles(&[1.0, 1.0, 2.0]);
        assert_eq!(a, [(0.0, 90.0), (90.0, 180.0), (180.0, 360.0)]);
        let spans: f64 = a.iter().map(|(s, e)| e - s).sum();
        assert_eq!(spans, 360.0);
        let one = pie_angles(&[5.0]);
        assert_eq!(one, [(0.0, 360.0)]);
    }

    #[test]
    fn pie_angles_of_thirds_do_not_drift() {
        let a = pie_angles(&[1.0, 1.0, 1.0]);
        assert_eq!(a[2].1, 360.0);
        assert!(
            a.windows(2).all(|w| w[0].1 == w[1].0),
            "slices touch exactly"
        );
        assert!((a[0].1 - 120.0).abs() < 1e-12);
    }

    #[test]
    fn pie_angles_with_a_zero_total_are_empty_slices() {
        assert_eq!(pie_angles(&[0.0, 0.0]), [(0.0, 0.0), (0.0, 0.0)]);
        assert!(pie_angles(&[]).is_empty());
    }
}
