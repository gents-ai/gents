//! Scatter and bubble charts, read back from their SVG.

use serde_json::json;

use crate::testkit::*;

fn points() -> String {
    with_rows(
        "scatter",
        r#""x":"h","y":"w""#,
        &["h", "w"],
        &[
            json!([150, 50]),
            json!([160, 58]),
            json!([170, 70]),
            json!([180, 82]),
        ],
    )
}

fn dots<'a, 'i>(doc: &'a roxmltree::Document<'i>) -> Vec<(f64, f64, f64)> {
    all(doc, "circle")
        .into_iter()
        .map(|c| (num(c, "cx"), num(c, "cy"), num(c, "r")))
        .collect()
}

#[test]
fn every_point_sits_where_the_axis_labels_put_its_values() {
    let r = ok(&points());
    let doc = parse(&r.svg);
    let (fx, fy) = (x_fit(&doc, &r.plot), y_fit(&doc, &r.plot));
    let d: Vec<_> = dots(&doc).into_iter().filter(|c| c.2 == 4.0).collect();
    assert_eq!(d.len(), 4);
    for ((x, y, _), (vx, vy)) in
        d.iter()
            .zip([(150.0, 50.0), (160.0, 58.0), (170.0, 70.0), (180.0, 82.0)])
    {
        assert!(
            (x - fx.px(vx)).abs() < 0.02 && (y - fy.px(vy)).abs() < 0.02,
            "({vx}, {vy}) at ({x}, {y})"
        );
    }
}

#[test]
fn points_have_tooltips_with_their_values_and_stay_inside_the_plot() {
    let r = ok(&points());
    let doc = parse(&r.svg);
    let tips: Vec<String> = all(&doc, "circle").into_iter().filter_map(title).collect();
    assert_eq!(
        tips,
        ["w: 150, 50", "w: 160, 58", "w: 170, 70", "w: 180, 82"]
    );
    for (x, y, _) in dots(&doc) {
        assert!(inside(&r.plot, x, y, 0.01));
    }
}

#[test]
fn a_series_column_makes_one_coloured_group_each_with_its_own_correlation() {
    let json = r#"{"chart":"scatter","x":"x","y":"y","series":"g","data":{"columns":["x","y","g"],"rows":[[1,2,"a"],[2,4,"a"],[3,6,"a"],[1,9,"b"],[2,6,"b"],[3,3,"b"]]}}"#;
    let r = ok(json);
    assert_eq!(
        r.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(r.series[0].extra["correlation"], json!(1.0));
    assert_eq!(r.series[1].extra["correlation"], json!(-1.0));
    assert_eq!(
        (
            r.series[0].extra["x_min"].clone(),
            r.series[0].extra["x_max"].clone()
        ),
        (json!(1.0), json!(3.0))
    );
    let doc = parse(&r.svg);
    let fills: Vec<&str> = all(&doc, "circle")
        .into_iter()
        .filter(|c| c.attribute("r") == Some("4"))
        .filter_map(|c| c.attribute("fill"))
        .collect();
    assert_eq!(
        fills,
        [
            "#0072b2", "#0072b2", "#0072b2", "#e69f00", "#e69f00", "#e69f00"
        ]
    );
    assert!(
        r.alt.contains("correlation 1") && r.alt.contains("correlation -1"),
        "{}",
        r.alt
    );
}

#[test]
fn dates_on_x_work_as_a_time_axis() {
    let rows = [
        json!(["2024-01-01", 1]),
        json!(["2024-07-01", 5]),
        json!(["2024-12-31", 3]),
    ];
    let r = ok(&with_rows("scatter", "", &["d", "v"], &rows));
    let t = texts(&parse(&r.svg));
    assert!(t.iter().any(|l| l.contains("2024")), "{t:?}");
    assert_eq!(r.series[0].points, 3);
}

#[test]
fn text_on_x_is_refused_with_advice() {
    let e = err(&with_rows(
        "scatter",
        "",
        &["k", "v"],
        &[json!(["a", 1]), json!(["b", 2])],
    ));
    assert!(
        e.contains("needs numbers or dates on x") && e.contains("bar chart"),
        "{e}"
    );
}

#[test]
fn log_axes_refuse_non_positive_values() {
    let rows = [json!([1, 5]), json!([0, 6])];
    assert!(
        err(&with_rows(
            "scatter",
            r#""x_scale":"log""#,
            &["a", "b"],
            &rows
        ))
        .contains("the x axis is logarithmic")
    );
    assert!(
        err(&with_rows(
            "scatter",
            r#""y_scale":"log""#,
            &["a", "b"],
            &[json!([1, 0])]
        ))
        .contains("the y axis is logarithmic")
    );
    let ok_rows = [json!([1, 10]), json!([10, 100]), json!([100, 1000])];
    let r = ok(&with_rows(
        "scatter",
        r#""x_scale":"log","y_scale":"log""#,
        &["a", "b"],
        &ok_rows,
    ));
    let d = dots(&parse(&r.svg));
    let (dx1, dx2) = (d[1].0 - d[0].0, d[2].0 - d[1].0);
    assert!(
        (dx1 - dx2).abs() < 0.05,
        "equal ratios, equal distances: {dx1} {dx2}"
    );
}

#[test]
fn nothing_to_draw_is_an_error_not_an_empty_chart() {
    let e = err(&with_rows(
        "scatter",
        "",
        &["a", "b"],
        &[json!([null, null]), json!(["x", "y"])],
    ));
    assert!(!e.is_empty() && !e.contains('\n'), "{e}");
}

#[test]
fn a_big_cloud_is_grouped_within_the_cap_keeping_the_extreme_points_exact() {
    let mut rows: Vec<_> = (0..20_000)
        .map(|i| json!([100 + (i * 7) % 90, 100 + (i * 13) % 70]))
        .collect();
    rows.push(json!([-500, 100]));
    rows.push(json!([700, 100]));
    rows.push(json!([150, -400]));
    rows.push(json!([150, 900]));
    let r = ok(&with_rows("scatter", "", &["x", "y"], &rows));
    let doc = parse(&r.svg);
    let d = dots(&doc);
    assert!(d.len() <= 10_000 && d.len() > 100, "{} marks", d.len());
    let (fx, fy) = (x_fit(&doc, &r.plot), y_fit(&doc, &r.plot));
    for (vx, vy) in [
        (-500.0, 100.0),
        (700.0, 100.0),
        (150.0, -400.0),
        (150.0, 900.0),
    ] {
        assert!(
            d.iter()
                .any(|c| (c.0 - fx.px(vx)).abs() < 0.02 && (c.1 - fy.px(vy)).abs() < 0.02),
            "extreme ({vx}, {vy}) is exact"
        );
    }
    assert!(
        r.warnings.iter().any(|w| w.contains("20004 points")
            && w.contains("grouped marks")
            && w.contains("extreme points stay exact")),
        "{:?}",
        r.warnings
    );
    assert_eq!(r.series[0].points, 20_004);
    assert!(r.series[0].drawn < 20_004);
    assert!(
        all(&doc, "circle").iter().all(|c| title(*c).is_none()),
        "no tooltips on a crowd"
    );
}

#[test]
fn x_and_y_bounds_limit_the_view() {
    let r = ok(&points().replace(
        r#""chart":"scatter""#,
        r#""chart":"scatter","x_min":140,"x_max":200,"y_min":0,"y_max":100"#,
    ));
    let doc = parse(&r.svg);
    let (fx, fy) = (x_fit(&doc, &r.plot), y_fit(&doc, &r.plot));
    assert!((fx.px(140.0) - r.plot.x).abs() < 0.02 && (fx.px(200.0) - r.plot.right()).abs() < 0.02);
    assert!((fy.px(0.0) - r.plot.bottom()).abs() < 0.02 && (fy.px(100.0) - r.plot.y).abs() < 0.02);
}

fn bubbles() -> String {
    with_rows(
        "bubble",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &[
            json!([1, 1, 100]),
            json!([2, 2, 400]),
            json!([3, 3, 900]),
            json!([4, 4, 100]),
        ],
    )
}

#[test]
fn bubble_area_is_proportional_to_the_size_value() {
    let r = ok(&bubbles());
    let d = dots(&parse(&r.svg));
    let mut radii: Vec<(f64, f64)> = d.iter().map(|c| (c.0, c.2)).collect();
    radii.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Sizes 100, 400, 900, 100: the area ratios are the value ratios.
    let area = |i: usize| radii[i].1 * radii[i].1;
    assert!(
        (area(1) / area(2) - 400.0 / 900.0).abs() < 1e-3,
        "{radii:?}"
    );
    assert!(
        (area(0) / area(2) - 100.0 / 900.0).abs() < 1e-3,
        "{radii:?}"
    );
    assert!(
        (area(0) - area(3)).abs() < 1e-9,
        "equal sizes, equal bubbles"
    );
}

#[test]
fn nearly_equal_sizes_draw_nearly_equal_bubbles() {
    let rows = [json!([1, 1, 100]), json!([2, 2, 101])];
    let r = ok(&with_rows(
        "bubble",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    let d = dots(&parse(&r.svg));
    let (a, b) = (d[0].2.min(d[1].2), d[0].2.max(d[1].2));
    assert!(b / a < 1.01, "radii {a} and {b}");
}

#[test]
fn bubbles_with_a_size_of_zero_or_below_are_not_drawn_and_counted() {
    let rows = [
        json!([1, 1, -5]),
        json!([2, 2, 0]),
        json!([3, 3, 10]),
        json!([4, 4, 40]),
    ];
    let r = ok(&with_rows(
        "bubble",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    assert_eq!(dots(&parse(&r.svg)).len(), 2);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("2 bubbles with a size of zero or below are not drawn")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn bubbles_need_at_least_one_size_above_zero() {
    let rows = [json!([1, 1, -5]), json!([2, 2, 0])];
    let e = err(&with_rows(
        "bubble",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    assert!(e.contains("no bubble has a size above zero"), "{e}");
}

#[test]
fn a_bubble_far_smaller_than_the_largest_is_floored_and_reported() {
    let rows = [json!([1, 1, 1]), json!([2, 2, 1_000_000])];
    let r = ok(&with_rows(
        "bubble",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    let d = dots(&parse(&r.svg));
    assert!(d.iter().any(|c| (c.2 - 1.5).abs() < 1e-9), "{d:?}");
    assert!(
        r.warnings
            .iter()
            .any(|w| w
                .contains("1 bubble too small to show to scale and drawn at the smallest size")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn the_biggest_bubbles_are_drawn_first_so_small_ones_stay_visible() {
    let r = ok(&bubbles());
    let d = dots(&parse(&r.svg));
    let radii: Vec<f64> = d.iter().map(|c| c.2).collect();
    assert!(radii.windows(2).all(|w| w[0] >= w[1]), "{radii:?}");
}

#[test]
fn bubbles_have_tooltips_with_their_size_and_the_description_names_the_size_column() {
    let r = ok(&bubbles());
    let doc = parse(&r.svg);
    let tips: Vec<String> = all(&doc, "circle").into_iter().filter_map(title).collect();
    assert!(tips.contains(&"b: 3, 3, size 900".to_owned()), "{tips:?}");
    assert!(
        r.alt.contains("Bubble size shows s, from 100 to 900"),
        "{}",
        r.alt
    );
    assert!(r.alt.starts_with("Bubble chart"));
}

#[test]
fn a_bubble_chart_needs_sizes_and_skips_rows_without_one() {
    assert!(
        err(&with_rows("bubble", "", &["a", "b"], &[json!([1, 2])]))
            .contains("needs a size column")
    );
    let rows = [json!([1, 1, 5]), json!([2, 2, null]), json!([3, 3, 9])];
    let r = ok(&with_rows(
        "bubble",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    assert_eq!(dots(&parse(&r.svg)).len(), 2);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("1 points have no size")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn size_is_ignored_for_scatter_with_a_note() {
    let rows = [json!([1, 1, 5]), json!([2, 2, 9])];
    let r = ok(&with_rows(
        "scatter",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("size only applies to bubble charts"))
    );
    assert!(dots(&parse(&r.svg)).iter().all(|c| c.2 == 4.0));
}

#[test]
fn more_than_two_thousand_bubbles_keep_the_largest() {
    let rows: Vec<_> = (0..2500).map(|i| json!([i, i % 97, i + 1])).collect();
    let r = ok(&with_rows(
        "bubble",
        r#""x":"a","y":"b","size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    assert_eq!(dots(&parse(&r.svg)).len(), 2000);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("2500 bubbles; the 2000 largest are drawn")),
        "{:?}",
        r.warnings
    );
    assert_eq!(r.series[0].points, 2000);
}
