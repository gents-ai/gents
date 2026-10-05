//! Property tests: invariants over random inputs. The seed is fixed, so a run
//! is reproducible and a failure is the same failure on every machine.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use serde_json::{Value, json};

use crate::testkit::*;
use crate::{dates, format, num, reduce, scale, stats, text};

fn cfg(cases: u32) -> ProptestConfig {
    Config {
        cases,
        rng_seed: RngSeed::Fixed(0x00C0_FFEE),
        failure_persistence: None,
        ..Config::default()
    }
}

/// A finite number of any magnitude from 1e-12 to 1e12, either sign.
fn magnitude() -> impl Strategy<Value = f64> {
    (-12i32..=12, 1.0f64..10.0, any::<bool>()).prop_map(|(e, m, neg)| {
        let v = m * num::pow10(e);
        if neg { -v } else { v }
    })
}

fn is_nice_step(step: f64) -> bool {
    let k = num::floor_log10(step);
    let m = step / num::pow10(k);
    [1.0, 2.0, 5.0, 10.0].iter().any(|n| (m - n).abs() < 1e-9)
}

proptest! {
    #![proptest_config(cfg(400))]

    #[test]
    fn ticks_are_increasing_evenly_spaced_nice_and_cover_the_range(a in magnitude(), b in magnitude(), target in 2usize..13) {
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        prop_assume!(hi - lo > 1e-9 * lo.abs().max(hi.abs()));
        let t = scale::nice_ticks(lo, hi, target);
        prop_assert!(t.values.len() >= 2 && t.values.len() <= 2 * target + 2, "{} ticks", t.values.len());
        prop_assert!(t.values.windows(2).all(|w| w[0] < w[1]), "increasing");
        for w in t.values.windows(2) {
            prop_assert!(((w[1] - w[0]) - t.step).abs() <= 1e-9 * t.step, "equal spacing {} vs {}", w[1] - w[0], t.step);
        }
        prop_assert!(is_nice_step(t.step), "step {}", t.step);
        let slack = 1e-8 * t.step;
        prop_assert!(t.values[0] <= lo + slack, "first tick {} covers {lo}", t.values[0]);
        prop_assert!(*t.values.last().unwrap() >= hi - slack, "last tick covers {hi}");
        for v in &t.values {
            let written = format!("{:.*}", t.decimals, v);
            prop_assert!(written.parse::<f64>().unwrap() == *v, "{v} needs no more than {} decimals", t.decimals);
        }
    }

    #[test]
    fn log_ticks_are_whole_decades_that_cover_the_range(a in 1i32..40, b in 1i32..40, ma in 1.0f64..9.99, mb in 1.0f64..9.99) {
        let (lo, hi) = (ma * num::pow10(-a.min(30)), mb * num::pow10(b - 20));
        prop_assume!(lo < hi);
        let t = scale::log_ticks(lo, hi);
        prop_assert!(t.lo <= lo && t.hi >= hi);
        prop_assert!(t.values.windows(2).all(|w| w[0] < w[1]));
        prop_assert!(t.values.len() <= 9, "{} ticks", t.values.len());
        for v in &t.values {
            let k = num::floor_log10(*v);
            let m = v / num::pow10(k);
            prop_assert!([1.0, 2.0, 5.0].iter().any(|n| (m - n).abs() < 1e-9), "{v}");
        }
        prop_assert!(t.values.contains(&t.hi));
    }

    #[test]
    fn linear_scales_are_monotonic_and_hit_their_end_points(d0 in -1e6f64..1e6, span in 1e-3f64..1e6, r0 in 0f64..2000.0, len in 10f64..2000.0, t in 0.0f64..1.0) {
        let s = scale::Scale::linear(d0, d0 + span, r0, r0 + len);
        prop_assert!((s.map(d0) - r0).abs() < 1e-6 && (s.map(d0 + span) - (r0 + len)).abs() < 1e-6);
        let inside = d0 + t * span;
        let p = s.map(inside);
        prop_assert!(p >= r0 - 1e-6 && p <= r0 + len + 1e-6);
        prop_assert!(s.map(inside) <= s.map(inside + span * 1e-3) + 1e-9);
    }

    #[test]
    fn calendar_ticks_cover_the_range_and_land_on_boundaries(start in -2_000_000_000i64..4_000_000_000, span in 60i64..2_000_000_000, target in 2usize..10) {
        let (lo, hi) = (start as f64, (start + span) as f64);
        let t = dates::ticks(lo, hi, target, None);
        prop_assert!(t.values.windows(2).all(|w| w[0] < w[1]));
        prop_assert!(t.values[0] <= lo && *t.values.last().unwrap() >= hi);
        prop_assert!(t.values.len() >= 2 && t.values.len() <= 3 * target + 3, "{} ticks", t.values.len());
        prop_assert!(t.labels.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn iso_timestamps_survive_a_parse_and_format_round_trip(y in 1i64..=9999, m in 1u32..=12, d in 1u32..=28, h in 0u32..24, mi in 0u32..60, s in 0u32..60) {
        let text = format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z");
        let t = dates::parse(&text).expect("a valid timestamp");
        prop_assert_eq!(dates::format(t, "%Y-%m-%dT%H:%M:%SZ"), text);
        let (cy, cm, cd) = dates::civil_from_days(dates::days_from_civil(y, m, d));
        prop_assert_eq!((cy, cm, cd), (y, m, d));
    }

    #[test]
    fn a_time_zone_offset_shifts_exactly_by_its_hours_and_minutes(h in 0u32..24, oh in 0u32..14, om in prop::sample::select(vec![0u32, 15, 30, 45]), west in any::<bool>()) {
        let sign = if west { '-' } else { '+' };
        let local = dates::parse(&format!("2024-03-10T{h:02}:00:00{sign}{oh:02}:{om:02}")).unwrap();
        let utc = dates::parse(&format!("2024-03-10T{h:02}:00:00Z")).unwrap();
        let shift = f64::from(oh * 3600 + om * 60);
        prop_assert_eq!(local, if west { utc + shift } else { utc - shift });
    }

    #[test]
    fn xml_escaping_is_reversible_and_leaves_no_markup(s in "\\PC{0,60}") {
        let e = text::escape(&s);
        prop_assert!(!e.contains(['<', '>', '"', '\'']));
        let mut back = String::new();
        let mut rest = e.as_str();
        while let Some(i) = rest.find('&') {
            back.push_str(&rest[..i]);
            rest = &rest[i..];
            let (ent, ch) = [("&amp;", '&'), ("&lt;", '<'), ("&gt;", '>'), ("&quot;", '"'), ("&#39;", '\'')].into_iter().find(|(ent, _)| rest.starts_with(ent)).expect("only the known entities");
            back.push(ch);
            rest = &rest[ent.len()..];
        }
        back.push_str(rest);
        prop_assert_eq!(back, s);
    }

    #[test]
    fn cleaned_text_has_no_control_characters(s in ".{0,60}") {
        let c = text::clean(&s);
        let clean = c.chars().all(|ch| !ch.is_control() && ch != '\u{FFFE}' && ch != '\u{FFFF}');
        prop_assert!(clean);
    }

    #[test]
    fn an_elided_text_fits_and_keeps_its_beginning(s in "[a-zA-Z0-9 ]{0,80}", max in 5.0f64..300.0) {
        let (shown, cut) = text::elide(&s, max, 12.0, false);
        if cut {
            let ellipsis = shown.ends_with('\u{2026}');
            prop_assert!(ellipsis);
            let stem = shown.trim_end_matches('\u{2026}');
            prop_assert!(s.starts_with(stem));
            if stem.is_empty() { return Ok(()); }
            prop_assert!(text::width(&shown, 12.0, false) <= max + 1e-9);
        } else {
            prop_assert_eq!(shown, s);
        }
    }

    #[test]
    fn fixed_formats_read_back_within_half_a_unit_of_the_last_digit(v in -1e9f64..1e9, d in 0usize..6) {
        let f = format::parse(&format!(",.{d}f")).unwrap();
        let s = f.apply(v, 0);
        let back: f64 = s.replace(',', "").parse().unwrap();
        prop_assert!((back - v).abs() <= 0.5 * num::pow10(-(d as i32)) + 1e-9, "{v} -> {s}");
    }

    #[test]
    fn percent_formats_read_back_to_the_same_fraction(v in -50.0f64..50.0, d in 0usize..4) {
        let s = format::parse(&format!(".{d}%")).unwrap().apply(v, 0);
        let back: f64 = s.trim_end_matches('%').parse::<f64>().unwrap() / 100.0;
        prop_assert!((back - v).abs() <= 0.5 * num::pow10(-(d as i32)) / 100.0 + 1e-12, "{v} -> {s}");
    }

    #[test]
    fn si_formats_read_back_within_a_percent(m in 1.0f64..9.99, e in -9i32..13, neg in any::<bool>()) {
        let v = m * num::pow10(e) * if neg { -1.0 } else { 1.0 };
        let s = format::parse("s").unwrap().apply(v, 0);
        let (digits, mult) = match s.chars().last().unwrap() {
            'k' => (&s[..s.len() - 1], 1e3),
            'M' => (&s[..s.len() - 1], 1e6),
            'G' => (&s[..s.len() - 1], 1e9),
            'T' => (&s[..s.len() - 1], 1e12),
            'm' => (&s[..s.len() - 1], 1e-3),
            '\u{b5}' => (&s[..s.len() - '\u{b5}'.len_utf8()], 1e-6),
            'n' => (&s[..s.len() - 1], 1e-9),
            _ => (s.as_str(), 1.0),
        };
        let back = digits.parse::<f64>().unwrap() * mult;
        prop_assert!(((back - v) / v).abs() < 0.01, "{v} -> {s}");
    }

    #[test]
    fn compact_text_is_short_and_close(v in magnitude()) {
        let s = format::compact(v);
        prop_assert!(s.len() <= 24, "{s}");
        let back: f64 = s.replace(',', "").parse().unwrap();
        prop_assert!(((back - v) / v).abs() < 1e-3, "{v} -> {s}");
    }
}

fn series_with_gaps() -> impl Strategy<Value = Vec<(f64, f64)>> {
    prop::collection::vec(
        prop_oneof![9 => (-1000.0f64..1000.0).prop_map(Some), 1 => Just(None)],
        0..800,
    )
    .prop_map(|ys| {
        ys.into_iter()
            .enumerate()
            .map(|(i, y)| (i as f64, y.unwrap_or(f64::NAN)))
            .collect()
    })
}

proptest! {
    #![proptest_config(cfg(300))]

    #[test]
    fn line_reduction_keeps_the_ends_the_extremes_and_never_a_gap(pts in series_with_gaps(), threshold in 3usize..120) {
        let keep = reduce::lttb(&pts, threshold);
        prop_assert!(keep.windows(2).all(|w| w[0] < w[1]), "ascending and unique");
        prop_assert!(keep.iter().all(|i| !pts[*i].1.is_nan()), "gaps are never drawn");
        let finite: Vec<usize> = (0..pts.len()).filter(|i| !pts[*i].1.is_nan()).collect();
        if finite.len() <= threshold {
            prop_assert_eq!(&keep, &finite, "small series are kept whole");
            return Ok(());
        }
        let mut i = 0;
        let mut runs = 0usize;
        while i < pts.len() {
            if pts[i].1.is_nan() { i += 1; continue; }
            let start = i;
            while i < pts.len() && !pts[i].1.is_nan() { i += 1; }
            runs += 1;
            prop_assert!(keep.contains(&start) && keep.contains(&(i - 1)), "run {start}..{i} keeps its ends");
            let (lo, hi) = (start..i).fold((start, start), |(lo, hi), k| (if pts[k].1 < pts[lo].1 { k } else { lo }, if pts[k].1 > pts[hi].1 { k } else { hi }));
            prop_assert!(keep.contains(&lo) && keep.contains(&hi), "run {start}..{i} keeps its lowest and highest");
        }
        prop_assert!(keep.len() <= threshold + 5 * runs, "{} kept for {threshold} over {runs} runs", keep.len());
    }

    #[test]
    fn scatter_grouping_counts_every_point_once_and_keeps_the_extremes(pts in prop::collection::vec((0.0f64..800.0, 0.0f64..480.0), 50..2500), cap in 20usize..400) {
        let b = reduce::bin_scatter(&pts, cap);
        let cells = &b.bins[..b.bins.len() - b.extremes];
        prop_assert_eq!(cells.iter().map(|c| c.n).sum::<usize>(), pts.len());
        if pts.len() > cap {
            let (min_x, max_x) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
            let (min_y, max_y) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
            for want in [min_x, max_x] { prop_assert!(b.bins.iter().any(|c| c.n == 1 && c.x == want)); }
            for want in [min_y, max_y] { prop_assert!(b.bins.iter().any(|c| c.n == 1 && c.y == want)); }
        }
        prop_assert!(b.bins.iter().all(|c| c.x >= 0.0 && c.x <= 800.0 && c.y >= 0.0 && c.y <= 480.0), "marks stay in the area");
    }

    #[test]
    fn stacked_layers_are_contiguous_and_the_top_is_the_column_sum(cols in prop::collection::vec(prop::collection::vec(0.0f64..1000.0, 1..8), 1..6)) {
        let n = cols.iter().map(Vec::len).min().unwrap();
        let series: Vec<Vec<f64>> = cols.iter().map(|c| c[..n].to_vec()).collect();
        let layers = stats::stack(&series);
        for i in 0..n {
            let sum: f64 = series.iter().map(|s| s[i]).sum();
            prop_assert!((layers.last().unwrap().hi[i] - sum).abs() < 1e-9 * (1.0 + sum), "top of column {i}");
            for k in 1..layers.len() {
                prop_assert_eq!(layers[k].lo[i], layers[k - 1].hi[i]);
            }
            prop_assert_eq!(layers[0].lo[i], 0.0);
        }
    }

    #[test]
    fn mixed_sign_stacks_keep_each_side_contiguous_from_zero(series in prop::collection::vec(prop::collection::vec(-100.0f64..100.0, 4), 1..6)) {
        let layers = stats::stack(&series);
        for i in 0..4 {
            let (mut pos, mut neg) = (0.0, 0.0);
            for (k, s) in series.iter().enumerate() {
                let v = s[i];
                let (lo, hi) = (layers[k].lo[i], layers[k].hi[i]);
                prop_assert!((hi - lo - v).abs() < 1e-9);
                if v >= 0.0 { prop_assert!((lo - pos).abs() < 1e-9); pos += v; } else { prop_assert!((lo - neg).abs() < 1e-9); neg += v; }
            }
        }
    }

    #[test]
    fn pie_angles_always_close_the_circle_without_gaps(values in prop::collection::vec(0.001f64..1e6, 1..40)) {
        let a = stats::pie_angles(&values);
        prop_assert_eq!(a[0].0, 0.0);
        prop_assert_eq!(a.last().unwrap().1, 360.0);
        prop_assert!(a.windows(2).all(|w| w[0].1 == w[1].0), "slices touch");
        prop_assert!(a.iter().all(|(s, e)| e >= s));
        let total: f64 = values.iter().sum();
        for ((s, e), v) in a.iter().zip(&values) {
            prop_assert!(((e - s) - v / total * 360.0).abs() < 1e-6, "span follows share");
        }
    }

    #[test]
    fn quartiles_are_ordered_between_the_extremes_and_outliers_lie_outside_the_whiskers(values in prop::collection::vec(-1e4f64..1e4, 1..200)) {
        let b = stats::box_stats(&values).unwrap();
        prop_assert!(b.min <= b.whisker_lo && b.whisker_lo <= b.q1 + 1e-9 && b.q1 <= b.median && b.median <= b.q3 && b.q3 <= b.whisker_hi + 1e-9 && b.whisker_hi <= b.max);
        prop_assert!(b.outliers.iter().all(|o| *o < b.whisker_lo || *o > b.whisker_hi));
        prop_assert_eq!(b.n, values.len());
        let inside = values.iter().filter(|v| **v >= b.whisker_lo && **v <= b.whisker_hi).count();
        prop_assert_eq!(inside + b.outliers.len(), values.len());
    }

    #[test]
    fn quantiles_never_decrease_with_q(mut values in prop::collection::vec(-1e3f64..1e3, 1..100), qa in 0.0f64..1.0, qb in 0.0f64..1.0) {
        values.sort_by(f64::total_cmp);
        let (lo, hi) = if qa < qb { (qa, qb) } else { (qb, qa) };
        prop_assert!(stats::quantile(&values, lo) <= stats::quantile(&values, hi) + 1e-9);
        prop_assert!(stats::quantile(&values, lo) >= values[0] - 1e-9 && stats::quantile(&values, hi) <= values[values.len() - 1] + 1e-9);
    }

    #[test]
    fn histogram_counts_add_up_to_the_values_inside_the_edges(values in prop::collection::vec(-100.0f64..100.0, 1..300), bins in 1usize..30) {
        let (lo, hi) = values.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
        let edges = stats::bin_edges(lo, hi, values.len(), crate::spec::Bins::Count(bins));
        prop_assert!(edges.windows(2).all(|w| w[0] < w[1]));
        let counts = stats::histogram(&values, &edges);
        prop_assert_eq!(counts.iter().sum::<u64>() as usize, values.len(), "every value falls in exactly one bin");
        let auto = stats::bin_edges(lo, hi, values.len(), crate::spec::Bins::Auto);
        prop_assert!(auto[0] <= lo && *auto.last().unwrap() >= hi);
        prop_assert_eq!(stats::histogram(&values, &auto).iter().sum::<u64>() as usize, values.len());
    }
}

fn cell_text() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-zA-Z][a-zA-Z0-9 ,;\"'\n\\-]{0,12}",
        "[a-z\u{e9}\u{4e2d}\u{1F600}]{1,6}",
        Just(String::new()),
        (-9999i32..9999).prop_map(|n| n.to_string()),
    ]
}

fn csv_quote(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

proptest! {
    #![proptest_config(cfg(200))]

    #[test]
    fn a_table_written_as_csv_reads_back_cell_for_cell(table in prop::collection::vec(prop::collection::vec(cell_text(), 3), 1..30)) {
        let mut csv = String::from("a,b,c\n");
        for row in &table {
            csv.push_str(&row.iter().map(|c| csv_quote(c)).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
        let mut b = crate::table::Builder::new(None);
        crate::csvio::read(csv.as_bytes(), &mut b).unwrap();
        let t = b.finish();
        let kept: Vec<&Vec<String>> = table.iter().collect();
        prop_assert_eq!(t.rows, kept.len());
        for (r, row) in kept.iter().enumerate() {
            for (c, want) in row.iter().enumerate() {
                let got = match t.cols[c][r] {
                    crate::table::Cell::Null => String::new(),
                    crate::table::Cell::Num(v) => format!("{v}"),
                    crate::table::Cell::Str(i) => t.pool.get(i).to_owned(),
                };
                let want_trim = want.trim();
                let null_like = ["null", "na", "n/a", "nan", "none"].iter().any(|m| want_trim.eq_ignore_ascii_case(m));
                if want_trim.is_empty() || null_like {
                    prop_assert_eq!(got, "", "empty and null-marker cells read as empty");
                } else if let Ok(n) = want_trim.parse::<i64>().map(|n| n as f64) {
                    if want_trim.len() > 1 && want_trim.trim_start_matches('-').starts_with('0') {
                        prop_assert_eq!(got, want_trim.to_owned());
                    } else {
                        prop_assert_eq!(got, format!("{n}"));
                    }
                } else {
                    prop_assert_eq!(got, want_trim.to_owned());
                }
            }
        }
    }

    #[test]
    fn rows_written_as_json_objects_read_back_in_order(rows in prop::collection::vec((cell_text(), -1e6f64..1e6), 1..40)) {
        let doc: Vec<Value> = rows.iter().map(|(s, n)| json!({"label": s, "n": n})).collect();
        let mut b = crate::table::Builder::new(None);
        crate::jsonio::read_str(&Value::Array(doc).to_string(), &mut b).unwrap();
        let t = b.finish();
        prop_assert_eq!(t.rows, rows.len());
        for (r, (_, n)) in rows.iter().enumerate() {
            match t.cols[t.col("n").unwrap()][r] {
                crate::table::Cell::Num(v) => prop_assert_eq!(v, *n),
                other => prop_assert!(false, "{other:?}"),
            }
        }
    }
}

fn plot_points() -> impl Strategy<Value = Vec<(f64, f64)>> {
    prop::collection::vec(
        (
            (-1000.0f64..1000.0).prop_map(|v| (v * 8.0).round() / 8.0),
            (-500.0f64..5000.0).prop_map(|v| (v * 8.0).round() / 8.0),
        ),
        1..60,
    )
}

proptest! {
    #![proptest_config(cfg(60))]

    #[test]
    fn every_scatter_point_lands_inside_the_plot_area(pts in plot_points(), w in 300u32..1200, h in 220u32..800) {
        let rows: Vec<Value> = pts.iter().map(|(x, y)| json!([x, y])).collect();
        let req = with_rows("scatter", &format!(r#""width":{w},"height":{h},"x":"x","y":"y""#), &["x", "y"], &rows);
        let r = ok(&req);
        let doc = parse(&r.svg);
        let dots: Vec<_> = all(&doc, "circle").into_iter().filter(|c| c.attribute("r") == Some("4")).collect();
        prop_assert_eq!(dots.len(), pts.len());
        for c in dots {
            prop_assert!(inside(&r.plot, num(c, "cx"), num(c, "cy"), 0.011), "({}, {}) outside {:?}", num(c, "cx"), num(c, "cy"), r.plot);
        }
    }

    #[test]
    fn line_and_bar_marks_stay_inside_the_plot_area(ys in prop::collection::vec(-1000i32..1000, 2..40), bars in any::<bool>()) {
        let rows: Vec<Value> = ys.iter().enumerate().map(|(i, y)| json!([format!("c{i}"), y])).collect();
        let r = ok(&with_rows(if bars { "bar" } else { "line" }, "", &["k", "v"], &rows));
        let doc = parse(&r.svg);
        if bars {
            for b in titled(&doc, "rect", "v, ") {
                let (x, y, w, h) = rect_of(b);
                prop_assert!(inside(&r.plot, x, y, 0.011) && inside(&r.plot, x + w, y + h, 0.011), "{x} {y} {w} {h} in {:?}", r.plot);
            }
        } else {
            for p in stroked_paths(&doc, "#0072b2").iter().flatten() {
                prop_assert!(inside(&r.plot, p.0, p.1, 0.011));
            }
        }
    }

    #[test]
    fn stacked_bars_reach_the_column_sums_on_the_axis(vals in prop::collection::vec(prop::collection::vec(1u32..900, 3), 1..8)) {
        let rows: Vec<Value> = vals.iter().enumerate().map(|(i, r)| json!([format!("c{i}"), r[0], r[1], r[2]])).collect();
        let r = ok(&with_rows("stacked_bar", "", &["k", "a", "b", "c"], &rows));
        let doc = parse(&r.svg);
        let fy = y_fit(&doc, &r.plot);
        let tops: Vec<f64> = titled(&doc, "rect", "c, ").iter().map(|b| rect_of(*b).1).collect();
        prop_assert_eq!(tops.len(), vals.len());
        for (top, row) in tops.iter().zip(&vals) {
            let sum = f64::from(row.iter().sum::<u32>());
            prop_assert!((top - fy.px(sum)).abs() < 0.05, "top {top} vs {}", fy.px(sum));
        }
    }

    #[test]
    fn the_same_request_always_gives_the_same_bytes(kind in 0usize..12, theme in any::<bool>(), legend in 0usize..6, w in 300u32..900, h in 240u32..600, seed in any::<u64>()) {
        let charts = ["line", "area", "stacked_area", "bar", "stacked_bar", "horizontal_bar", "scatter", "histogram", "box", "pie", "donut", "heatmap"];
        let legends = ["auto", "none", "right", "bottom", "top", "left"];
        let mut s = seed;
        let mut next = move || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s % 1000) as i64 };
        let rows: Vec<Value> = (0..12).map(|i| json!([format!("k{}", i % 5), next(), next().abs() + 1, next().abs() + 1])).collect();
        let extra = format!(r#""theme":"{}","legend":"{}","width":{w},"height":{h},"title":"T","x":"k","y":["a","b"],"value":"b""#, if theme { "dark" } else { "light" }, legends[legend]);
        let extra = match charts[kind] {
            "scatter" => extra.replace(r#""x":"k""#, r#""x":"a""#).replace(r#""y":["a","b"]"#, r#""y":["b"]"#),
            "histogram" => extra.replace(r#""x":"k""#, r#""x":"a""#),
            "heatmap" => extra.replace(r#""y":["a","b"]"#, r#""y":["b"]"#).replace(r#""value":"b""#, r#""value":"c""#),
            "box" => extra.replace(r#""y":["a","b"]"#, r#""y":["a"]"#),
            "pie" | "donut" => extra.replace(r#""y":["a","b"]"#, r#""y":["b"]"#),
            _ => extra,
        };
        let req = with_rows(charts[kind], &extra, &["k", "a", "b", "c"], &rows);
        let first = crate::run(&req);
        let second = crate::run(&req);
        prop_assert_eq!(first, second);
    }
}
