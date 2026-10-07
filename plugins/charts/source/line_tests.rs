//! Line, area and stacked area charts, read back from their SVG.

use serde_json::json;

use crate::spec::MAX_LINE_POINTS;
use crate::testkit::*;

const BLUE: &str = "#0072b2";
const ORANGE: &str = "#e69f00";

fn numeric_line() -> String {
    with_rows(
        "line",
        r#""x":"t","y":["a"]"#,
        &["t", "a"],
        &[
            json!([0, 10]),
            json!([5, 20]),
            json!([10, 15]),
            json!([20, 40]),
        ],
    )
}

#[test]
fn points_sit_where_the_axis_labels_say_their_values_are() {
    let r = ok(&numeric_line());
    let doc = parse(&r.svg);
    let (fy, fx) = (y_fit(&doc, &r.plot), x_fit(&doc, &r.plot));
    let paths = stroked_paths(&doc, BLUE);
    assert_eq!(paths.len(), 1);
    let want = [(0.0, 10.0), (5.0, 20.0), (10.0, 15.0), (20.0, 40.0)];
    assert_eq!(paths[0].len(), want.len());
    for (p, (x, y)) in paths[0].iter().zip(want) {
        assert!(
            (p.0 - fx.px(x)).abs() < 0.02,
            "x of {x}: {} vs {}",
            p.0,
            fx.px(x)
        );
        assert!(
            (p.1 - fy.px(y)).abs() < 0.02,
            "y of {y}: {} vs {}",
            p.1,
            fy.px(y)
        );
    }
}

#[test]
fn a_numeric_x_axis_spans_exactly_the_data_so_the_line_fills_the_plot() {
    let r = ok(&numeric_line());
    let doc = parse(&r.svg);
    let p = &stroked_paths(&doc, BLUE)[0];
    assert!((p[0].0 - r.plot.x).abs() < 0.02);
    assert!((p[p.len() - 1].0 - r.plot.right()).abs() < 0.02);
}

#[test]
fn rows_out_of_order_are_drawn_left_to_right() {
    let json = with_rows(
        "line",
        r#""x":"t","y":["a"]"#,
        &["t", "a"],
        &[json!([10, 1]), json!([0, 2]), json!([5, 3])],
    );
    let r = ok(&json);
    let doc = parse(&r.svg);
    let xs: Vec<f64> = stroked_paths(&doc, BLUE)[0].iter().map(|p| p.0).collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
}

#[test]
fn category_x_keeps_the_order_of_the_data() {
    let json = with_rows(
        "line",
        "",
        &["m", "v"],
        &[json!(["Mar", 3]), json!(["Jan", 1]), json!(["Feb", 2])],
    );
    let r = ok(&json);
    let doc = parse(&r.svg);
    let labels: Vec<String> = texts(&doc)
        .into_iter()
        .filter(|t| ["Mar", "Jan", "Feb"].contains(&t.as_str()))
        .collect();
    assert_eq!(labels, ["Mar", "Jan", "Feb"]);
    let p = &stroked_paths(&doc, BLUE)[0];
    assert!(p.windows(2).all(|w| w[0].0 < w[1].0));
}

#[test]
fn a_missing_value_breaks_the_line_and_is_reported_not_drawn_as_zero() {
    let json = with_rows(
        "line",
        "",
        &["t", "v"],
        &[
            json!([1, 10]),
            json!([2, 12]),
            json!([3, null]),
            json!([4, 14]),
            json!([5, 15]),
        ],
    );
    let r = ok(&json);
    let doc = parse(&r.svg);
    let runs = stroked_paths(&doc, BLUE);
    assert_eq!(runs.len(), 2, "two runs around the gap");
    assert_eq!((runs[0].len(), runs[1].len()), (2, 2));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("1 empty values are drawn as gaps, not zeros")),
        "{:?}",
        r.warnings
    );
    assert_eq!((r.series[0].points, r.series[0].gaps), (4, 1));
    // The y axis does not reach for zero because of the gap.
    let fy = y_fit(&doc, &r.plot);
    assert!(
        fy.px(0.0) > r.plot.bottom(),
        "zero is below the plot: the axis starts above it"
    );
}

#[test]
fn a_non_numeric_value_is_a_reported_gap_too() {
    let json = with_rows(
        "line",
        "",
        &["t", "v"],
        &[json!([1, 10]), json!([2, "n/a?"]), json!([3, 12])],
    );
    let r = ok(&json);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("not numbers") && w.contains("\"n/a?\" in row 2")),
        "{:?}",
        r.warnings
    );
    assert_eq!(
        stroked_paths(&parse(&r.svg), BLUE).len(),
        0,
        "two isolated points make no line"
    );
    let doc = parse(&r.svg);
    let radii: Vec<&str> = all(&doc, "circle")
        .into_iter()
        .filter_map(|c| c.attribute("r"))
        .collect();
    assert_eq!(
        radii.iter().filter(|r| **r == "2.5").count(),
        2,
        "each isolated point is a dot: {radii:?}"
    );
}

#[test]
fn a_single_point_is_a_dot_not_an_empty_line() {
    let r = ok(&with_rows("line", "", &["t", "v"], &[json!([1, 10])]));
    let doc = parse(&r.svg);
    assert!(stroked_paths(&doc, BLUE).is_empty());
    assert!(!all(&doc, "circle").is_empty());
    assert_eq!(r.series[0].points, 1);
}

#[test]
fn two_series_get_two_colours_a_legend_and_one_line_each() {
    let json = with_rows(
        "line",
        "",
        &["t", "a", "b"],
        &[json!([1, 1, 5]), json!([2, 2, 4]), json!([3, 3, 6])],
    );
    let r = ok(&json);
    let doc = parse(&r.svg);
    assert_eq!(stroked_paths(&doc, BLUE).len(), 1);
    assert_eq!(stroked_paths(&doc, ORANGE).len(), 1);
    let legend: Vec<String> = texts(&doc)
        .into_iter()
        .filter(|t| t == "a" || t == "b")
        .collect();
    assert_eq!(legend, ["a", "b"]);
    assert_eq!(
        r.series
            .iter()
            .map(|s| s.color.as_str())
            .collect::<Vec<_>>(),
        [BLUE, ORANGE]
    );
}

#[test]
fn a_single_series_has_no_legend_unless_asked() {
    // The y label is off so the series name can only come from a legend.
    let json = numeric_line().replace(r#""chart":"line""#, r#""chart":"line","y_label":"""#);
    let none = ok(&json);
    assert!(!texts(&parse(&none.svg)).contains(&"a".to_owned()));
    let shown = ok(&json.replace(r#""chart":"line""#, r#""chart":"line","legend":"right""#));
    assert!(texts(&parse(&shown.svg)).contains(&"a".to_owned()));
}

#[test]
fn legend_placement_moves_the_legend_around_the_plot() {
    let base = with_rows(
        "line",
        "",
        &["t", "a", "b"],
        &[json!([1, 1, 5]), json!([2, 2, 4]), json!([3, 3, 6])],
    );
    let legend_pos = |placement: &str| {
        let r = ok(&base.replace(
            r#""chart":"line""#,
            &format!(r#""chart":"line","legend":"{placement}""#),
        ));
        let doc = parse(&r.svg);
        let t = all(&doc, "text")
            .into_iter()
            .find(|n| n.text() == Some("a"))
            .map(|n| (num(n, "x"), num(n, "y")))
            .unwrap();
        (t, r.plot)
    };
    let ((x, _), plot) = legend_pos("right");
    assert!(x > plot.right(), "right legend sits right of the plot");
    let ((x, _), plot) = legend_pos("left");
    assert!(x < plot.x, "left legend sits left of the plot");
    let ((_, y), plot) = legend_pos("top");
    assert!(y < plot.y, "top legend sits above the plot");
    let ((_, y), plot) = legend_pos("bottom");
    assert!(y > plot.bottom(), "bottom legend sits below the plot");
    let r = ok(&base.replace(r#""chart":"line""#, r#""chart":"line","legend":"none""#));
    assert!(!texts(&parse(&r.svg)).contains(&"a".to_owned()));
}

#[test]
fn few_points_get_markers_with_tooltips_and_many_do_not() {
    let r = ok(&numeric_line());
    let doc = parse(&r.svg);
    let tips: Vec<String> = all(&doc, "circle").into_iter().filter_map(title).collect();
    assert_eq!(tips, ["a: 0, 10", "a: 5, 20", "a: 10, 15", "a: 20, 40"]);
    let rows: Vec<_> = (0..80).map(|i| json!([i, i * 2])).collect();
    let many = ok(&with_rows("line", "", &["t", "v"], &rows));
    assert!(all(&parse(&many.svg), "circle").is_empty());
}

#[test]
fn axis_titles_default_to_the_columns_and_can_be_overridden_or_removed() {
    let json = numeric_line();
    let r = ok(&json);
    let t = texts(&parse(&r.svg));
    assert!(t.contains(&"t".to_owned()) && t.contains(&"a".to_owned()));
    let r = ok(&json.replace(
        r#""chart":"line""#,
        r#""chart":"line","x_label":"Time (s)","y_label":"Level""#,
    ));
    let t = texts(&parse(&r.svg));
    assert!(t.contains(&"Time (s)".to_owned()) && t.contains(&"Level".to_owned()));
    let r = ok(&json.replace(
        r#""chart":"line""#,
        r#""chart":"line","x_label":"","y_label":"""#,
    ));
    let t = texts(&parse(&r.svg));
    assert!(
        !t.contains(&"t".to_owned()) && !t.contains(&"a".to_owned()),
        "{t:?}"
    );
}

#[test]
fn y_bounds_fix_the_axis_and_clip_the_marks() {
    let json = numeric_line().replace(
        r#""chart":"line""#,
        r#""chart":"line","y_min":0,"y_max":100"#,
    );
    let r = ok(&json);
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    assert!((fy.px(0.0) - r.plot.bottom()).abs() < 0.02);
    assert!((fy.px(100.0) - r.plot.y).abs() < 0.02);
    assert_eq!(all(&doc, "clipPath").len(), 1);
    assert!(
        all(&doc, "g")
            .iter()
            .any(|g| g.attribute("clip-path").is_some())
    );
}

#[test]
fn a_log_axis_places_equal_ratios_at_equal_distances() {
    let rows = [
        json!([1, 1]),
        json!([2, 10]),
        json!([3, 100]),
        json!([4, 1000]),
    ];
    let r = ok(&with_rows("line", r#""y_scale":"log""#, &["t", "v"], &rows));
    let doc = parse(&r.svg);
    let p = &stroked_paths(&doc, BLUE)[0];
    let steps: Vec<f64> = p.windows(2).map(|w| w[0].1 - w[1].1).collect();
    assert!(
        steps.iter().all(|s| (s - steps[0]).abs() < 0.05),
        "{steps:?}"
    );
    let tick_labels: Vec<String> = texts(&doc)
        .into_iter()
        .filter(|t| ["1", "10", "100", "1,000"].contains(&t.as_str()))
        .collect();
    assert!(tick_labels.contains(&"1,000".to_owned()), "{tick_labels:?}");
}

#[test]
fn a_log_axis_refuses_zero_and_negative_values_naming_the_first() {
    let rows = [json!([1, 5]), json!([2, 0]), json!([3, -4])];
    let e = err(&with_rows("line", r#""y_scale":"log""#, &["t", "v"], &rows));
    assert!(
        e.contains("column \"v\" has 2 values at or below zero")
            && e.contains("first is 0 in row 2"),
        "{e}"
    );
    let e = err(&with_rows(
        "line",
        r#""x_scale":"log""#,
        &["t", "v"],
        &[json!([0, 1]), json!([2, 2])],
    ));
    assert!(e.contains("the x axis is logarithmic"), "{e}");
}

#[test]
fn a_log_axis_is_refused_for_area_charts() {
    let e = err(&with_rows(
        "area",
        r#""y_scale":"log""#,
        &["t", "v"],
        &[json!([1, 1]), json!([2, 2])],
    ));
    assert!(e.contains("works for line charts"), "{e}");
}

#[test]
fn dates_on_x_make_a_time_axis_with_calendar_labels() {
    let rows = [
        json!(["2024-01-15", 1]),
        json!(["2024-06-15", 2]),
        json!(["2024-12-15", 3]),
    ];
    let r = ok(&with_rows("line", "", &["d", "v"], &rows));
    let doc = parse(&r.svg);
    let t = texts(&doc);
    assert!(
        t.iter().any(|l| l == "Jul 2024"),
        "month ticks with the year: {t:?}"
    );
    let p = &stroked_paths(&doc, BLUE)[0];
    // Jan 15 to Jun 15 is 152 days, Jun 15 to Dec 15 is 183: the x gaps follow the calendar.
    let (g1, g2) = (p[1].0 - p[0].0, p[2].0 - p[1].0);
    assert!((g1 / g2 - 152.0 / 183.0).abs() < 0.002, "{g1} {g2}");
    assert!(
        r.alt.contains("2024-01-15") && r.alt.contains("2024-12-15"),
        "{}",
        r.alt
    );
}

#[test]
fn a_time_axis_can_use_a_date_pattern() {
    let rows = [json!(["2020-01-01", 1]), json!(["2024-01-01", 2])];
    let r = ok(&with_rows(
        "line",
        r#""x_format":"'%y""#,
        &["d", "v"],
        &rows,
    ));
    let t = texts(&parse(&r.svg));
    assert!(t.iter().any(|l| l == "'22"), "{t:?}");
}

#[test]
fn years_on_a_numeric_axis_have_no_thousands_separator() {
    let rows = [
        json!([2019, 5]),
        json!([2020, 6]),
        json!([2021, 7]),
        json!([2022, 9]),
    ];
    let r = ok(&with_rows("line", "", &["year", "v"], &rows));
    let t = texts(&parse(&r.svg));
    assert!(t.iter().any(|l| l == "2020"), "{t:?}");
    assert!(!t.iter().any(|l| l.contains("2,0")), "{t:?}");
}

#[test]
fn a_long_series_is_reduced_keeping_its_extremes_and_says_so() {
    let n = 6000;
    let rows: Vec<_> = (0..n)
        .map(|i| {
            json!([
                i,
                if i == 1234 {
                    500
                } else if i == 4321 {
                    -300
                } else {
                    (i % 50) as i64
                }
            ])
        })
        .collect();
    let r = ok(&with_rows("line", "", &["t", "v"], &rows));
    let doc = parse(&r.svg);
    let p = &stroked_paths(&doc, BLUE)[0];
    assert!(p.len() <= 1502 && p.len() > 700, "{} points", p.len());
    let fy = y_fit(&doc, &r.plot);
    let ys: Vec<f64> = p.iter().map(|q| q.1).collect();
    assert!(
        ys.iter().any(|y| (y - fy.px(500.0)).abs() < 0.05),
        "the spike survives"
    );
    assert!(
        ys.iter().any(|y| (y - fy.px(-300.0)).abs() < 0.05),
        "the dip survives"
    );
    assert!((p[0].0 - r.plot.x).abs() < 0.05 && (p[p.len() - 1].0 - r.plot.right()).abs() < 0.05);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("has 6000 points") && w.contains("largest-triangle")),
        "{:?}",
        r.warnings
    );
    assert_eq!(r.series[0].points, 6000);
    assert!(r.series[0].drawn > 700 && r.series[0].drawn <= 1502);
}

#[test]
fn an_area_chart_fills_down_to_the_zero_line() {
    let rows = [json!([0, 10]), json!([1, 30]), json!([2, 20])];
    let r = ok(&with_rows("area", "", &["t", "v"], &rows));
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let fill = all(&doc, "path")
        .into_iter()
        .find(|p| p.attribute("fill") == Some(BLUE) && p.attribute("fill-opacity").is_some())
        .expect("an area path");
    let pts = path_points(fill.attribute("d").unwrap());
    let base = fy.px(0.0);
    assert!(
        (pts[0].1 - base).abs() < 0.02 && (pts[pts.len() - 1].1 - base).abs() < 0.02,
        "{pts:?}"
    );
    assert!(
        r.plot.bottom() - base < 0.02,
        "the axis starts at zero for an area chart"
    );
}

#[test]
fn stacked_areas_pile_up_and_the_top_is_the_column_sum() {
    let rows = [
        json!([0, 1, 2, 3]),
        json!([1, 4, 5, 6]),
        json!([2, 2, 2, 2]),
    ];
    let r = ok(&with_rows("stacked_area", "", &["t", "a", "b", "c"], &rows));
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let fills: Vec<Vec<(f64, f64)>> = all(&doc, "path")
        .into_iter()
        .filter(|p| p.attribute("fill-opacity") == Some("0.85"))
        .map(|p| path_points(p.attribute("d").unwrap()))
        .collect();
    assert_eq!(fills.len(), 3);
    // Each layer's outline starts with its top edge, left to right: three points.
    let tops: Vec<Vec<f64>> = fills
        .iter()
        .map(|f| f[..3].iter().map(|p| p.1).collect())
        .collect();
    let sums = [[6.0, 15.0, 6.0], [3.0, 9.0, 4.0], [1.0, 4.0, 2.0]];
    for (layer, want) in tops.iter().rev().zip(sums) {
        for (y, v) in layer.iter().zip(want) {
            assert!((y - fy.px(v)).abs() < 0.02, "{y} vs {}", fy.px(v));
        }
    }
}

#[test]
fn a_stacked_series_with_no_value_counts_as_zero_and_says_so() {
    let json = r#"{"chart":"stacked_area","x":"t","y":"v","series":"s","data":{"columns":["t","s","v"],"rows":[[1,"a",1],[2,"a",2],[3,"a",3],[1,"b",5],[3,"b",6]]}}"#;
    let r = ok(json);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("\"b\" has no value at 1 of 3 positions") && w.contains("zero")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_long_stack_is_drawn_at_reduced_positions_that_keep_its_extremes() {
    let rows: Vec<serde_json::Value> = (0..20_000)
        .map(|i| {
            let peak = if i == 12_345 { 1000 } else { i % 7 };
            let dip = if i == 777 { -500 } else { 1 };
            json!([i, peak, dip])
        })
        .collect();
    let r = ok(&with_rows("stacked_area", "", &["t", "a", "b"], &rows));
    let drawn = r.series[0].drawn;
    assert!(drawn > 2 && drawn <= MAX_LINE_POINTS + 4, "{drawn}");
    assert_eq!(r.series[0].points, 20_000);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("20000 x positions") && w.contains(&format!("{drawn} are drawn"))),
        "{:?}",
        r.warnings
    );
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let tops: Vec<f64> = all(&doc, "path")
        .into_iter()
        .filter(|p| p.attribute("fill-opacity") == Some("0.85"))
        .flat_map(|p| path_points(p.attribute("d").unwrap()))
        .map(|p| p.1)
        .collect();
    let highest = tops.iter().copied().fold(f64::INFINITY, f64::min);
    let lowest = tops.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (highest - fy.px(1001.0)).abs() < 0.02,
        "the peak total is drawn"
    );
    assert!((lowest - fy.px(-500.0)).abs() < 0.02, "the dip is drawn");
}

#[test]
fn a_spike_offset_by_another_layer_survives_the_reduction() {
    // a spikes up at one x while b falls by the same amount: the total stays flat.
    let rows: Vec<serde_json::Value> = (0..20_000)
        .map(|i| {
            let (a, b) = if i == 9_999 { (500, 0) } else { (10, 490) };
            json!([i, a, b])
        })
        .collect();
    let r = ok(&with_rows("stacked_area", "", &["t", "a", "b"], &rows));
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let layers: Vec<Vec<(f64, f64)>> = all(&doc, "path")
        .into_iter()
        .filter(|p| p.attribute("fill-opacity") == Some("0.85"))
        .map(|p| path_points(p.attribute("d").unwrap()))
        .collect();
    let spike = layers[0].iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    assert!(
        (spike - fy.px(500.0)).abs() < 0.02,
        "the spike of a is drawn"
    );
}

#[test]
fn long_data_pivots_into_one_line_per_series_value() {
    let json = r#"{"chart":"line","x":"t","y":"v","series":"s","data":{"columns":["t","s","v"],"rows":[[1,"a",1],[2,"a",2],[1,"b",5],[2,"b",3]]}}"#;
    let r = ok(json);
    assert_eq!(
        r.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(r.series[1].min, Some(3.0));
    assert_eq!(r.series[1].max, Some(5.0));
}

#[test]
fn duplicate_categories_are_combined_by_agg() {
    let rows = [json!(["a", 1]), json!(["a", 3]), json!(["b", 5])];
    let r = ok(&with_rows("line", r#""agg":"mean""#, &["k", "v"], &rows));
    assert_eq!((r.series[0].min, r.series[0].max), (Some(2.0), Some(5.0)));
}

#[test]
fn the_dark_theme_changes_the_background_and_text_colours_not_the_geometry() {
    let light = ok(&numeric_line());
    let dark = ok(&numeric_line().replace(r#""chart":"line""#, r#""chart":"line","theme":"dark""#));
    let (dl, dd) = (parse(&light.svg), parse(&dark.svg));
    assert_eq!(all(&dl, "rect")[0].attribute("fill"), Some("#ffffff"));
    assert_eq!(all(&dd, "rect")[0].attribute("fill"), Some("#14181d"));
    assert_eq!(light.plot, dark.plot);
    assert_eq!(
        stroked_paths(&dd, "#56b4e9").len(),
        1,
        "the dark palette starts with the lighter blue"
    );
}

#[test]
fn the_description_names_axes_series_and_extremes() {
    let r = ok(&numeric_line().replace(
        r#""chart":"line""#,
        r#""chart":"line","title":"Level over time""#,
    ));
    assert!(
        r.alt.starts_with("Line chart titled \"Level over time\"."),
        "{}",
        r.alt
    );
    assert!(r.alt.contains("X axis: t, from 0 to 20."), "{}", r.alt);
    assert!(r.alt.contains("1 series."));
    assert!(
        r.alt
            .contains("\"a\": 4 points, lowest 10 at 0, highest 40 at 20."),
        "{}",
        r.alt
    );
    let doc = parse(&r.svg);
    let root = doc.root_element();
    assert_eq!(all(&doc, "title")[0].text(), Some("Level over time"));
    assert_eq!(all(&doc, "desc")[0].text(), Some(r.alt.as_str()));
    assert_eq!(root.attribute("role"), Some("img"));
}
