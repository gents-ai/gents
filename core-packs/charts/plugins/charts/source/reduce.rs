//! Reducing large point sets for drawing: largest-triangle-three-buckets for
//! lines, which keeps the first, last, lowest and highest point of every run,
//! and grid binning for scatter plots, which keeps the extreme points.

/// Indices to draw for `pts` (x, y), ascending. A point whose y is `NaN` is a
/// gap: it is never drawn and splits the series into runs. A run of at most
/// its share of `threshold` points is kept whole; a longer run keeps its
/// first and last point, its lowest and highest y, and the points the
/// triangle-area rule picks, so a reduced run has at most its share plus two
/// points.
pub fn lttb(pts: &[(f64, f64)], threshold: usize) -> Vec<usize> {
    let finite = pts
        .iter()
        .filter(|p| !p.1.is_nan() && p.0.is_finite())
        .count();
    let threshold = threshold.max(3);
    let mut out = Vec::new();
    let mut i = 0;
    while i < pts.len() {
        if pts[i].1.is_nan() || !pts[i].0.is_finite() {
            i += 1;
            continue;
        }
        let start = i;
        while i < pts.len() && !pts[i].1.is_nan() && pts[i].0.is_finite() {
            i += 1;
        }
        let run = &pts[start..i];
        let share = if finite <= threshold {
            run.len()
        } else {
            (threshold * run.len()).div_ceil(finite).max(3)
        };
        if run.len() <= share {
            out.extend(start..i);
        } else {
            out.extend(run_indices(run, share).into_iter().map(|k| k + start));
        }
    }
    out
}

fn run_indices(run: &[(f64, f64)], target: usize) -> Vec<usize> {
    let n = run.len();
    let buckets = target - 2;
    let interior = n - 2;
    let edge = |b: usize| 1 + b * interior / buckets;
    let mut keep = vec![0];
    let mut prev = 0;
    for b in 0..buckets {
        let (lo, hi) = (edge(b), edge(b + 1));
        let (nlo, nhi) = if b + 1 < buckets {
            (hi, edge(b + 2))
        } else {
            (n - 1, n)
        };
        let count = (nhi - nlo) as f64;
        let (sx, sy) = run[nlo..nhi]
            .iter()
            .fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
        let (ax, ay) = (sx / count, sy / count);
        let (px, py) = run[prev];
        let mut best = lo;
        let mut best_area = -1.0;
        for (k, p) in run.iter().enumerate().take(hi).skip(lo) {
            let area = ((px - ax) * (p.1 - py) - (px - p.0) * (ay - py)).abs();
            if area > best_area {
                best_area = area;
                best = k;
            }
        }
        keep.push(best);
        prev = best;
    }
    keep.push(n - 1);
    let (mut lo_i, mut hi_i) = (0, 0);
    for (k, p) in run.iter().enumerate() {
        if p.1 < run[lo_i].1 {
            lo_i = k;
        }
        if p.1 > run[hi_i].1 {
            hi_i = k;
        }
    }
    keep.push(lo_i);
    keep.push(hi_i);
    keep.sort_unstable();
    keep.dedup();
    keep
}

/// A group of scatter points drawn as one mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bin {
    /// Mean x of the members.
    pub x: f64,
    /// Mean y of the members.
    pub y: f64,
    /// Members.
    pub n: usize,
}

/// The result of [`bin_scatter`].
#[derive(Debug, Clone, PartialEq)]
pub struct Binned {
    /// Cell bins in cell order, then the exact extreme points.
    pub bins: Vec<Bin>,
    /// Cell size in pixels; 0 when no grouping was needed.
    pub cell: f64,
    /// How many bins at the end are exact extreme points that are also
    /// counted inside a cell bin.
    pub extremes: usize,
}

/// Groups points given in pixels into the smallest square grid whose number
/// of occupied cells is at most `max_out` less the four extremes. The points
/// with the lowest and highest x and y are then added as single-point bins so
/// the extremes stay exact.
pub fn bin_scatter(pts: &[(f64, f64)], max_out: usize) -> Binned {
    if pts.len() <= max_out {
        return Binned {
            bins: pts
                .iter()
                .map(|p| Bin {
                    x: p.0,
                    y: p.1,
                    n: 1,
                })
                .collect(),
            cell: 0.0,
            extremes: 0,
        };
    }
    let sizes = [
        2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0, 96.0, 128.0,
    ];
    let (min_x, min_y) = pts.iter().fold((f64::INFINITY, f64::INFINITY), |m, p| {
        (m.0.min(p.0), m.1.min(p.1))
    });
    let key = |p: &(f64, f64), size: f64| {
        let cx = ((p.0 - min_x) / size).floor() as u64;
        let cy = ((p.1 - min_y) / size).floor() as u64;
        (cy << 32) | cx
    };
    let mut chosen = sizes[sizes.len() - 1];
    let mut keys: Vec<(u64, usize)> = Vec::new();
    for size in sizes {
        keys = pts
            .iter()
            .enumerate()
            .map(|(i, p)| (key(p, size), i))
            .collect();
        keys.sort_unstable();
        let occupied = 1 + keys.windows(2).filter(|w| w[0].0 != w[1].0).count();
        if occupied <= max_out.saturating_sub(4) {
            chosen = size;
            break;
        }
    }
    let mut bins: Vec<Bin> = Vec::new();
    let mut i = 0;
    while i < keys.len() {
        let mut j = i;
        let (mut sx, mut sy) = (0.0, 0.0);
        while j < keys.len() && keys[j].0 == keys[i].0 {
            sx += pts[keys[j].1].0;
            sy += pts[keys[j].1].1;
            j += 1;
        }
        let n = j - i;
        bins.push(Bin {
            x: sx / n as f64,
            y: sy / n as f64,
            n,
        });
        i = j;
    }
    let mut extreme = [0usize; 4];
    for (k, p) in pts.iter().enumerate() {
        if p.0 < pts[extreme[0]].0 {
            extreme[0] = k;
        }
        if p.0 > pts[extreme[1]].0 {
            extreme[1] = k;
        }
        if p.1 < pts[extreme[2]].1 {
            extreme[2] = k;
        }
        if p.1 > pts[extreme[3]].1 {
            extreme[3] = k;
        }
    }
    extreme.sort_unstable();
    let mut last = usize::MAX;
    let mut extremes = 0;
    for k in extreme {
        if k != last {
            bins.push(Bin {
                x: pts[k].0,
                y: pts[k].1,
                n: 1,
            });
            extremes += 1;
            last = k;
        }
    }
    Binned {
        bins,
        cell: chosen,
        extremes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(n: usize, f: impl Fn(usize) -> f64) -> Vec<(f64, f64)> {
        (0..n).map(|i| (i as f64, f(i))).collect()
    }

    #[test]
    fn small_inputs_are_kept_whole() {
        let p = series(10, |i| i as f64);
        assert_eq!(lttb(&p, 100), (0..10).collect::<Vec<_>>());
        assert_eq!(lttb(&p, 10), (0..10).collect::<Vec<_>>());
        assert_eq!(lttb(&[], 10), Vec::<usize>::new());
        assert_eq!(lttb(&[(1.0, 2.0)], 3), [0]);
    }

    #[test]
    fn a_long_series_shrinks_to_about_the_threshold_and_keeps_its_ends() {
        let p = series(10_000, |i| ((i * 37) % 101) as f64);
        let k = lttb(&p, 500);
        assert!(k.len() <= 502, "{}", k.len());
        assert!(k.len() >= 250, "{}", k.len());
        assert_eq!(k[0], 0);
        assert_eq!(*k.last().unwrap(), 9_999);
        assert!(k.windows(2).all(|w| w[0] < w[1]), "ascending and unique");
    }

    #[test]
    fn the_global_lowest_and_highest_points_always_survive() {
        let mut p = series(5_000, |i| (i as f64 / 50.0).sin_approx());
        p[1234].1 = 77.0;
        p[4321].1 = -55.0;
        let k = lttb(&p, 200);
        assert!(k.contains(&1234) && k.contains(&4321));
    }

    trait SinApprox {
        fn sin_approx(self) -> f64;
    }
    impl SinApprox for f64 {
        fn sin_approx(self) -> f64 {
            crate::det::sin_cos(self).0
        }
    }

    #[test]
    fn a_spike_in_flat_data_is_kept() {
        let mut p = series(1_000, |_| 0.0);
        p[500].1 = 10.0;
        let k = lttb(&p, 20);
        assert!(k.contains(&500));
    }

    #[test]
    fn gaps_split_runs_and_are_never_drawn() {
        let mut p = series(600, |i| i as f64);
        for point in &mut p[200..260] {
            point.1 = f64::NAN;
        }
        let k = lttb(&p, 100);
        assert!(k.iter().all(|i| !(200..260).contains(i)));
        assert!(
            k.contains(&199) && k.contains(&260),
            "both ends of the gap survive"
        );
        assert_eq!(k[0], 0);
        assert_eq!(*k.last().unwrap(), 599);
    }

    #[test]
    fn a_series_of_only_gaps_has_no_points() {
        let p = vec![(0.0, f64::NAN); 5];
        assert!(lttb(&p, 3).is_empty());
        assert!(lttb(&[(f64::NAN, 1.0)], 3).is_empty());
    }

    #[test]
    fn every_run_keeps_its_own_first_and_last_point() {
        let mut p = series(3_000, |i| (i % 17) as f64);
        for g in [1000, 2000] {
            p[g].1 = f64::NAN;
        }
        let k = lttb(&p, 90);
        for must in [0, 999, 1001, 1999, 2001, 2999] {
            assert!(k.contains(&must), "{must}");
        }
    }

    #[test]
    fn the_triangle_rule_picks_the_corner_of_a_v_shape() {
        let p = [
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 10.0),
            (4.0, 0.0),
            (5.0, 0.0),
            (6.0, 0.0),
        ];
        assert_eq!(lttb(&p, 3), [0, 3, 6]);
    }

    #[test]
    fn reduction_is_deterministic() {
        let p = series(7_777, |i| ((i * 7919) % 1000) as f64);
        assert_eq!(lttb(&p, 321), lttb(&p, 321));
    }

    #[test]
    fn scatter_below_the_cap_is_returned_as_it_is() {
        let pts = [(1.0, 2.0), (3.0, 4.0)];
        let b = bin_scatter(&pts, 10);
        assert_eq!((b.cell, b.extremes), (0.0, 0));
        assert_eq!(
            b.bins,
            [
                Bin {
                    x: 1.0,
                    y: 2.0,
                    n: 1
                },
                Bin {
                    x: 3.0,
                    y: 4.0,
                    n: 1
                }
            ]
        );
    }

    #[test]
    fn dense_scatter_is_grouped_within_the_cap_and_loses_no_point() {
        let pts: Vec<(f64, f64)> = (0..50_000_u64)
            .map(|i| (((i * 7919) % 800) as f64, ((i * 104_729) % 480) as f64))
            .collect();
        let b = bin_scatter(&pts, 3_000);
        assert!(b.cell > 0.0);
        assert!(b.bins.len() <= 3_000, "{}", b.bins.len());
        let cells = &b.bins[..b.bins.len() - b.extremes];
        assert_eq!(
            cells.iter().map(|c| c.n).sum::<usize>(),
            50_000,
            "every point is in exactly one cell"
        );
        assert!(b.extremes >= 1 && b.extremes <= 4);
    }

    #[test]
    fn the_extreme_points_stay_exact() {
        let mut pts: Vec<(f64, f64)> = (0..20_000)
            .map(|i| (100.0 + (i % 50) as f64, 100.0 + (i % 37) as f64))
            .collect();
        pts[17] = (-5.0, 130.0);
        pts[9000] = (999.0, 120.0);
        pts[12_345] = (120.0, -33.0);
        pts[19_999] = (110.0, 4_000.0);
        let b = bin_scatter(&pts, 500);
        for want in [
            (-5.0, 130.0),
            (999.0, 120.0),
            (120.0, -33.0),
            (110.0, 4_000.0),
        ] {
            assert!(
                b.bins.iter().any(|b| b.n == 1 && (b.x, b.y) == want),
                "{want:?}"
            );
        }
    }

    #[test]
    fn binning_is_deterministic_and_ordered_by_cell() {
        let pts: Vec<(f64, f64)> = (0..9_000)
            .map(|i| (((i * 31) % 400) as f64, ((i * 17) % 300) as f64))
            .collect();
        assert_eq!(bin_scatter(&pts, 700), bin_scatter(&pts, 700));
    }

    #[test]
    fn identical_points_collapse_into_one_bin() {
        let pts = vec![(5.0, 5.0); 1_000];
        let b = bin_scatter(&pts, 10);
        assert_eq!(
            b.bins[0],
            Bin {
                x: 5.0,
                y: 5.0,
                n: 1_000
            }
        );
        assert!(b.bins.len() <= 5);
    }
}
