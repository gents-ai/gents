//! Drawing helpers for point-based marks: splitting a series into runs at
//! gaps, lines, filled areas and point markers.

use crate::svg::{PathData, Style, Svg};

/// Splits `pts` (pixels, `NaN` y for a gap) into runs of drawn points. Only
/// the points at indices `keep` are used; two kept points are in different
/// runs when a gap lies between them in `pts`.
pub fn split_runs(pts: &[(f64, f64)], keep: &[usize]) -> Vec<Vec<(f64, f64)>> {
    let mut gaps = Vec::with_capacity(pts.len() + 1);
    let mut n = 0usize;
    gaps.push(0);
    for p in pts {
        n += usize::from(p.1.is_nan());
        gaps.push(n);
    }
    let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut prev: Option<usize> = None;
    for &i in keep {
        let p = pts[i];
        if p.1.is_nan() {
            continue;
        }
        let joined = prev.is_some_and(|a| gaps[i] == gaps[a + 1]);
        if joined {
            if let Some(run) = runs.last_mut() {
                run.push(p);
            }
        } else {
            runs.push(vec![p]);
        }
        prev = Some(i);
    }
    runs
}

/// A polyline through one run.
pub fn line_path(run: &[(f64, f64)]) -> PathData {
    let mut d = PathData::new();
    for (i, p) in run.iter().enumerate() {
        if i == 0 {
            d.move_to(p.0, p.1);
        } else {
            d.line_to(p.0, p.1);
        }
    }
    d
}

/// A closed shape from a run down to the horizontal `base` pixel.
pub fn area_path(run: &[(f64, f64)], base: f64) -> PathData {
    let mut d = PathData::new();
    if let (Some(first), Some(last)) = (run.first(), run.last()) {
        d.move_to(first.0, base);
        for p in run {
            d.line_to(p.0, p.1);
        }
        d.line_to(last.0, base);
        d.close();
    }
    d
}

/// Draws the runs of a series as lines; a run of one point is a dot.
pub fn draw_lines(svg: &mut Svg, runs: &[Vec<(f64, f64)>], color: &str, width: f64) {
    for run in runs {
        if run.len() == 1 {
            svg.circle(run[0].0, run[0].1, width * 1.25, &Style::fill(color));
        } else {
            svg.path(&line_path(run), &Style::stroke(color, width).rounded());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: f64 = f64::NAN;

    #[test]
    fn a_gap_splits_a_series_into_runs() {
        let pts = [(0.0, 1.0), (1.0, 2.0), (2.0, N), (3.0, 4.0), (4.0, 5.0)];
        let runs = split_runs(&pts, &[0, 1, 2, 3, 4]);
        assert_eq!(runs, [vec![(0.0, 1.0), (1.0, 2.0)], vec![(3.0, 4.0), (4.0, 5.0)]]);
    }

    #[test]
    fn a_reduced_series_still_breaks_where_the_data_has_a_gap() {
        let pts = [(0.0, 1.0), (1.0, 2.0), (2.0, 3.0), (3.0, N), (4.0, 5.0), (5.0, 6.0)];
        let runs = split_runs(&pts, &[0, 2, 4, 5]);
        assert_eq!(runs, [vec![(0.0, 1.0), (2.0, 3.0)], vec![(4.0, 5.0), (5.0, 6.0)]]);
    }

    #[test]
    fn kept_points_without_a_gap_between_them_stay_joined() {
        let pts: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), f64::from(i))).collect();
        assert_eq!(split_runs(&pts, &[0, 4, 9]).len(), 1);
    }

    #[test]
    fn isolated_points_are_runs_of_one_and_empty_input_has_no_runs() {
        let pts = [(0.0, 1.0), (1.0, N), (2.0, 3.0), (3.0, N), (4.0, 5.0)];
        let runs = split_runs(&pts, &[0, 1, 2, 3, 4]);
        assert_eq!(runs.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 1]);
        assert!(split_runs(&[], &[]).is_empty());
    }

    #[test]
    fn line_and_area_paths_have_the_expected_commands() {
        let run = [(1.0, 5.0), (2.0, 3.0), (3.0, 4.0)];
        assert_eq!(line_path(&run).as_str(), "M 1,5 L 2,3 L 3,4");
        assert_eq!(area_path(&run, 10.0).as_str(), "M 1,10 L 1,5 L 2,3 L 3,4 L 3,10 Z");
        assert!(area_path(&[], 10.0).is_empty());
    }

    #[test]
    fn a_single_point_is_drawn_as_a_dot_not_a_line() {
        let mut s = Svg::new(50.0, 50.0, "#fff");
        draw_lines(&mut s, &[vec![(5.0, 5.0)], vec![(1.0, 1.0), (9.0, 9.0)]], "#f00", 2.0);
        let out = s.finish("t", "d").0;
        assert_eq!(out.matches("<circle").count(), 1);
        assert_eq!(out.matches("<path").count(), 1);
        assert!(out.contains("stroke-linejoin=\"round\""));
    }
}
