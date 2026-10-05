//! One targeted assertion per fault the mutation review found alive: each
//! test pins a boundary or a value the earlier suites left free, with the
//! expected figures worked out by hand in the comment beside it.

use serde_json::{Value, json};

use crate::testkit::*;
use crate::{dates, stats, table};

// ---- stats -------------------------------------------------------------

#[test]
fn the_whiskers_reach_values_up_to_one_and_a_half_interquartile_ranges_out() {
    // 1..=9 and 14: q1 = 3.25, q3 = 7.75, iqr = 4.5, so the fences are
    // 3.25 - 6.75 = -3.5 and 7.75 + 6.75 = 14.5. 14 is 1.39 iqr past q3.
    let b = stats::box_stats(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 14.0]).unwrap();
    assert_eq!((b.q1, b.q3), (3.25, 7.75));
    assert_eq!(b.whisker_hi, 14.0);
    assert!(b.outliers.is_empty(), "{:?}", b.outliers);
    // Mirrored below q1: -4 is 1.39 iqr under it... the fence is -3.5, so -3 stays and -4 goes.
    let low = stats::box_stats(&[-3.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]).unwrap();
    assert_eq!(low.whisker_lo, -3.0, "{low:?}");
    assert!(low.outliers.is_empty(), "{:?}", low.outliers);
    // 15 is past the high fence of 14.5, so it is an outlier and the whisker stops at 9.
    let out = stats::box_stats(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 15.0]).unwrap();
    assert_eq!((out.whisker_hi, out.outliers.clone()), (9.0, vec![15.0]));
}

#[test]
fn automatic_histogram_bins_follow_sturges_rule() {
    use crate::spec::Bins;
    // Sturges: ceil(log2 n) + 1 bins, so 5, 8 and 11 for 10, 100 and 1000
    // values. nice_ticks picks the step from range / bins: 0..36 over 5 is
    // 7.2 (step 10); 0..60 over 8 is 7.5 (step 10); 0..80 over 11 is 7.3
    // (step 10). One more bin would give steps of 5.
    let edges = |max: f64, n: usize| stats::bin_edges(0.0, max, n, Bins::Auto);
    assert_eq!(edges(36.0, 10), [0.0, 10.0, 20.0, 30.0, 40.0]);
    assert_eq!(edges(60.0, 100), [0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0]);
    assert_eq!(
        edges(80.0, 1000),
        [0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0]
    );
    // Very few values still get the five-bin minimum.
    assert_eq!(edges(36.0, 1), edges(36.0, 10));
    assert_eq!(edges(36.0, 2), edges(36.0, 10));
}

// ---- reduce ------------------------------------------------------------

#[test]
fn binned_scatter_keeps_room_for_its_four_extreme_points() {
    // A 10 x 10 lattice with spacing 2 fills 100 cells at the smallest cell
    // size, and 20 repeats of one point push the count over the cap of 100.
    // The four extremes are drawn on top, so cells may use at most 96.
    let mut pts: Vec<(f64, f64)> = (0..10)
        .flat_map(|i| (0..10).map(move |j| (f64::from(i) * 2.0, f64::from(j) * 2.0)))
        .collect();
    pts.extend(std::iter::repeat_n((0.0, 0.0), 20));
    let b = crate::reduce::bin_scatter(&pts, 100);
    assert!(b.extremes >= 2);
    assert!(
        b.bins.len() <= 100,
        "{} marks for a cap of 100",
        b.bins.len()
    );
    assert!(
        b.cell > 2.0,
        "the smallest cells would need 100 marks: {}",
        b.cell
    );
}

// ---- dates -------------------------------------------------------------

fn day(s: &str) -> f64 {
    dates::parse(s).unwrap_or_else(|| panic!("{s} parses"))
}

#[test]
fn quarterly_ticks_fall_on_the_first_of_january_april_july_and_october() {
    let t = dates::ticks(day("2023-02-10"), day("2025-02-10"), 8, None);
    assert_eq!(
        t.labels,
        [
            "Jan 2023", "Apr 2023", "Jul 2023", "Oct 2023", "Jan 2024", "Apr 2024", "Jul 2024",
            "Oct 2024", "Jan 2025", "Apr 2025"
        ]
    );
    assert_eq!(t.values[0], day("2023-01-01"));
}

#[test]
fn half_year_ticks_fall_on_the_first_of_january_and_july() {
    let t = dates::ticks(day("2023-08-15"), day("2027-08-15"), 8, None);
    assert_eq!(t.labels.first().map(String::as_str), Some("Jul 2023"));
    assert_eq!(t.labels.get(1).map(String::as_str), Some("Jan 2024"));
    assert!(
        t.labels
            .iter()
            .all(|l| l.starts_with("Jan") || l.starts_with("Jul")),
        "{:?}",
        t.labels
    );
}

#[test]
fn the_clock_runs_from_00_00_to_23_59_and_24_00_is_not_a_time() {
    assert!(dates::parse("2024-01-01T23:59").is_some());
    assert!(dates::parse("2024-01-01T23:59:59").is_some());
    assert!(dates::parse("2024-01-01T24:00").is_none());
    assert!(dates::parse("2024-01-01T24:00:00").is_none());
    assert!(dates::parse("2024-01-01T12:60").is_none());
    assert!(dates::parse("2024-01-01T12:00:60").is_none());
    assert!(dates::parse("2024-01-01 24:30").is_none());
}

// ---- table and column kinds -------------------------------------------

#[test]
fn a_leading_zero_before_more_digits_keeps_text_a_label() {
    for t in ["05", "00", "007", "-05", "+05", "00.5", "0123"] {
        assert_eq!(table::plain_number(t), None, "{t}");
    }
    for (t, v) in [
        ("0", 0.0),
        ("0.5", 0.5),
        ("-0", 0.0),
        ("10", 10.0),
        ("5", 5.0),
    ] {
        assert_eq!(table::plain_number(t), Some(v), "{t}");
    }
}

fn column(cells: &[&str]) -> table::Table {
    let mut b = table::Builder::new(None);
    b.set_header(&["c".to_owned()]);
    for c in cells {
        let cell = b.text_cell(c);
        b.push_positional(&[cell]);
    }
    b.finish()
}

#[test]
fn a_column_is_numeric_when_numbers_are_at_least_as_many_as_text() {
    use crate::cols::{ColKind, kind};
    assert_eq!(
        kind(&column(&["1", "2", "a", "b"]), 0),
        ColKind::Numeric,
        "half numbers"
    );
    assert_eq!(kind(&column(&["1", "a", "b"]), 0), ColKind::Text);
    assert_eq!(kind(&column(&["1", "2", "3", "a"]), 0), ColKind::Numeric);
    assert_eq!(kind(&column(&[]), 0), ColKind::Empty);
}

#[test]
fn a_column_is_time_only_when_every_text_cell_is_a_date() {
    use crate::cols::{ColKind, kind};
    assert_eq!(kind(&column(&["2024-01-01", "hello"]), 0), ColKind::Text);
    assert_eq!(
        kind(&column(&["2024-01-01", "2024-02-01"]), 0),
        ColKind::Time
    );
    assert_eq!(kind(&column(&["2024-01-01", "x", "y"]), 0), ColKind::Text);
}

// ---- chart limits ------------------------------------------------------

fn categories(n: usize) -> Vec<Value> {
    (0..n).map(|i| json!([format!("k{i:02}"), i + 1])).collect()
}

#[test]
fn a_pie_of_exactly_twelve_slices_is_not_grouped_and_thirteen_is() {
    let twelve = ok(&with_rows("pie", "", &["k", "v"], &categories(12)));
    assert!(twelve.alt.contains("12 slices"), "{}", twelve.alt);
    assert!(!twelve.alt.contains("Other"), "{}", twelve.alt);
    assert!(
        twelve.warnings.iter().all(|w| !w.contains("grouped")),
        "{:?}",
        twelve.warnings
    );
    let thirteen = ok(&with_rows("pie", "", &["k", "v"], &categories(13)));
    assert!(
        thirteen.alt.contains("12 slices") && thirteen.alt.contains("\"Other\""),
        "{}",
        thirteen.alt
    );
    assert!(
        thirteen
            .warnings
            .iter()
            .any(|w| w == "there are 13 slices; the 2 smallest are grouped as \"Other\""),
        "{:?}",
        thirteen.warnings
    );
}

#[test]
fn forty_box_groups_are_all_drawn_and_forty_one_are_cut_with_a_warning() {
    let rows = |n: usize| -> Vec<Value> {
        (0..n)
            .flat_map(|i| (0..4).map(move |j| json!([format!("g{i:02}"), i * 10 + j])))
            .collect()
    };
    let forty = ok(&with_rows(
        "box",
        r#""x":"g","y":["v"]"#,
        &["g", "v"],
        &rows(40),
    ));
    assert!(forty.alt.contains("40 boxes"), "{}", forty.alt);
    assert!(
        forty.warnings.iter().all(|w| !w.contains("groups")),
        "{:?}",
        forty.warnings
    );
    let more = ok(&with_rows(
        "box",
        r#""x":"g","y":["v"]"#,
        &["g", "v"],
        &rows(41),
    ));
    assert!(more.alt.contains("40 boxes"), "{}", more.alt);
    assert!(
        more.warnings
            .iter()
            .any(|w| w == "there are 41 groups; only the first 40 are drawn"),
        "{:?}",
        more.warnings
    );
}

#[test]
fn two_thousand_bubbles_are_all_drawn_and_two_thousand_and_one_keep_the_largest() {
    let rows = |n: usize| -> Vec<Value> { (0..n).map(|i| json!([i, i % 97, i + 1])).collect() };
    let cols = ["a", "b", "s"];
    let extra = r#""x":"a","y":["b"],"size":"s","output":"svg""#;
    let all = ok(&with_rows("bubble", extra, &cols, &rows(2000)));
    assert_eq!(dots(&parse(&all.svg)).len(), 2000);
    assert!(
        all.warnings.iter().all(|w| !w.contains("largest")),
        "{:?}",
        all.warnings
    );
    let over = ok(&with_rows("bubble", extra, &cols, &rows(2001)));
    assert_eq!(dots(&parse(&over.svg)).len(), 2000);
    assert!(
        over.warnings
            .iter()
            .any(|w| w.contains("has 2001 bubbles; the 2000 largest are drawn")),
        "{:?}",
        over.warnings
    );
}

/// The radius of every circle in a chart's plot (legend swatches left out).
fn dots(doc: &roxmltree::Document<'_>) -> Vec<f64> {
    all(doc, "circle")
        .into_iter()
        .map(|c| num(c, "r"))
        .collect()
}

#[test]
fn scatter_dots_shrink_once_there_are_more_than_a_thousand_points() {
    let rows = |n: usize| -> Vec<Value> { (0..n).map(|i| json!([i, (i * 7) % 50])).collect() };
    let extra = r#""x":"a","y":["b"],"output":"svg""#;
    let thousand = ok(&with_rows("scatter", extra, &["a", "b"], &rows(1000)));
    assert!(dots(&parse(&thousand.svg)).iter().all(|r| *r == 4.0));
    assert_eq!(dots(&parse(&thousand.svg)).len(), 1000);
    let more = ok(&with_rows("scatter", extra, &["a", "b"], &rows(1001)));
    let d = dots(&parse(&more.svg));
    assert_eq!(d.len(), 1001);
    assert!(d.iter().all(|r| *r == 2.5), "{:?}", &d[..3]);
}

/// Value labels drawn on bars: text elements equal to one of `values`.
fn value_labels(r: &crate::output::Rendered, values: &[String]) -> usize {
    texts(&parse(&r.svg))
        .iter()
        .filter(|t| values.contains(t))
        .count()
}

#[test]
fn bars_are_labelled_up_to_thirty_and_not_beyond_or_when_too_narrow() {
    let rows = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|i| json!([format!("c{i}"), 101.5 + i as f64]))
            .collect()
    };
    let values =
        |n: usize| -> Vec<String> { (0..n).map(|i| format!("{}", 101.5 + i as f64)).collect() };
    let wide = |n: usize| {
        ok(&with_rows(
            "bar",
            r#""x":"k","y":["v"],"width":1800,"output":"svg""#,
            &["k", "v"],
            &rows(n),
        ))
    };
    assert_eq!(value_labels(&wide(30), &values(30)), 30);
    assert_eq!(value_labels(&wide(31), &values(31)), 0);
    // 20 bars in a narrow image are thinner than 22 px, so they carry no labels;
    // the same 20 in a wide one do.
    let narrow = ok(&with_rows(
        "bar",
        r#""x":"k","y":["v"],"width":400,"output":"svg""#,
        &["k", "v"],
        &rows(20),
    ));
    assert_eq!(value_labels(&narrow, &values(20)), 0);
    let roomy = ok(&with_rows(
        "bar",
        r#""x":"k","y":["v"],"width":900,"output":"svg""#,
        &["k", "v"],
        &rows(20),
    ));
    assert_eq!(value_labels(&roomy, &values(20)), 20);
}

#[test]
fn grouped_bars_count_every_bar_of_every_group_against_the_limit_of_thirty() {
    let rows: Vec<Value> = (0..15)
        .map(|i| json!([format!("c{i}"), 101.5 + i as f64, 201.5 + i as f64]))
        .collect();
    let both: Vec<String> = (0..15)
        .flat_map(|i| {
            [
                format!("{}", 101.5 + i as f64),
                format!("{}", 201.5 + i as f64),
            ]
        })
        .collect();
    let r = ok(&with_rows(
        "bar",
        r#""x":"k","y":["a","b"],"width":2400,"output":"svg""#,
        &["k", "a", "b"],
        &rows,
    ));
    assert_eq!(value_labels(&r, &both), 30);
    let rows: Vec<Value> = (0..16)
        .map(|i| json!([format!("c{i}"), 101.5 + i as f64, 201.5 + i as f64]))
        .collect();
    let r = ok(&with_rows(
        "bar",
        r#""x":"k","y":["a","b"],"width":2400,"output":"svg""#,
        &["k", "a", "b"],
        &rows,
    ));
    assert_eq!(value_labels(&r, &both), 0, "32 bars are too many to label");
}

// ---- request and graph limits -----------------------------------------

#[test]
fn twenty_four_value_columns_are_drawn_and_twenty_five_are_refused() {
    let cols = |n: usize| -> Vec<String> {
        std::iter::once("k".to_owned())
            .chain((0..n).map(|i| format!("v{i}")))
            .collect()
    };
    let build = |n: usize| {
        let columns = cols(n);
        let refs: Vec<&str> = columns.iter().map(String::as_str).collect();
        let row: Vec<Value> = std::iter::once(json!("a"))
            .chain((0..n).map(|i| json!(i)))
            .collect();
        let ys = serde_json::to_string(&columns[1..]).unwrap();
        with_rows(
            "bar",
            &format!(r#""x":"k","y":{ys},"output":"svg""#),
            &refs,
            &[json!(row)],
        )
    };
    assert_eq!(ok(&build(24)).series.len(), 24);
    let e = err(&build(25));
    assert_eq!(e, "at most 24 value columns can be drawn; name fewer in y");
}

#[test]
fn a_graph_record_with_every_optional_text_field_empty_is_an_unset_request() {
    let raw = json!({
        "run_id": "r", "chart": "bar", "x": "k", "y": ["v"],
        "legend": "", "theme": "", "agg": "", "sort": "", "stack": "", "output": "",
        "x_scale": "", "y_scale": "", "y2_scale": "", "line_axis": "", "bins": "",
        "save": "", "size": "", "value": "", "series": "", "subtitle": "", "title": "",
        "x_label": "", "y_label": "", "y2_label": "", "x_format": "", "y_format": "",
        "y2_format": "", "file": "", "line": [], "colors": [],
        "data": {"columns": ["k", "v"], "rows": [["a", 1], ["b", 2]]}
    });
    let v: Value = serde_json::from_str(&crate::run(&raw.to_string()).unwrap()).unwrap();
    assert!(v.get("error").is_none(), "{v}");
    assert_eq!(v["chart"], "bar");
    assert!(v["svg"].as_str().is_some_and(|s| s.starts_with("<svg")));
}

// ---- text and glyphs ---------------------------------------------------

#[test]
fn a_cut_label_ends_at_the_last_character_that_fits_with_the_ellipsis() {
    use crate::text::{elide, width};
    // Advances in the font are multiples of 1/2048 em, so at 16 px every
    // width below is exact in binary and the boundary is exact.
    let size = 16.0;
    let (digit, dots) = (width("0", size, false), width("\u{2026}", size, false));
    let max = dots + 3.0 * digit;
    let (out, cut) = elide("0000000", max, size, false);
    assert!(cut);
    assert_eq!(out, "000\u{2026}", "three digits fill the room exactly");
    assert_eq!(width(&out, size, false), max);
    let (out, _) = elide("0000000", max - 1e-9, size, false);
    assert_eq!(out, "00\u{2026}");
    let (same, cut) = elide("000", max, size, false);
    assert_eq!((same.as_str(), cut), ("000", false));
}

#[test]
fn eight_missing_glyphs_are_all_named_and_more_are_counted() {
    let cjk: Vec<char> =
        "\u{65e5}\u{6708}\u{706b}\u{6c34}\u{6728}\u{91d1}\u{571f}\u{5e74}\u{5929}\u{5730}"
            .chars()
            .collect();
    let note = |n: usize| -> String {
        let title: String = cjk[..n].iter().collect();
        let rows = [json!(["a", 1])];
        let extra = format!(
            r#""title":{},"x":"k","y":["v"]"#,
            serde_json::to_string(&title).unwrap()
        );
        let r = ok(&with_rows("bar", &extra, &["k", "v"], &rows));
        r.warnings
            .iter()
            .find(|w| w.contains("no glyph"))
            .cloned()
            .unwrap_or_default()
    };
    // Only run the check when the font really lacks these characters.
    if note(8).is_empty() {
        return;
    }
    let eight = note(8);
    assert!(eight.starts_with("8 characters have no glyph"), "{eight}");
    assert!(!eight.contains(" more"), "{eight}");
    let nine = note(9);
    assert!(
        nine.starts_with("9 characters") && nine.ends_with(" and 1 more"),
        "{nine}"
    );
    let ten = note(10);
    assert!(ten.ends_with(" and 2 more"), "{ten}");
    let listed = ten
        .split(": ")
        .nth(1)
        .unwrap_or_default()
        .split(" and ")
        .next()
        .unwrap_or_default();
    assert_eq!(listed.split(' ').count(), 8, "only 8 are listed: {ten}");
}

// ---- axes --------------------------------------------------------------

#[test]
fn a_whole_number_axis_starts_at_the_whole_number_below_its_data() {
    use crate::axes::{NumSpec, numeric};
    use crate::frame::AxisKind;
    let a = numeric(
        NumSpec {
            min: 2.5,
            max: 4.5,
            user_min: None,
            user_max: None,
            zero: false,
            nice: true,
            log: false,
            integer: true,
        },
        12,
        None,
        None,
    );
    let AxisKind::Cont { d0, d1, ticks, .. } = a.kind else {
        panic!("a continuous axis")
    };
    let labels: Vec<&str> = ticks.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(labels, ["2", "3", "4", "5"]);
    assert_eq!((d0, d1), (2.0, 5.0));
}
