//! Histograms and box plots, read back from their SVG.

use serde_json::json;

use crate::testkit::*;

fn values(vs: &[i64]) -> Vec<serde_json::Value> {
    vs.iter().map(|v| json!([v])).collect()
}

#[test]
fn histogram_bars_have_the_height_of_their_bin_count() {
    let sample = [1, 2, 2, 3, 3, 3, 4, 4, 5, 9];
    let r = ok(&with_rows(
        "histogram",
        r#""bins":3"#,
        &["v"],
        &values(&sample),
    ));
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let counts: Vec<u64> = serde_json::from_value(r.series[0].extra["counts"].clone()).unwrap();
    // Three equal bins over [1, 9]: [1, 3.67) holds 1, 2, 2, 3, 3, 3; [3.67, 6.33) holds 4, 4, 5; [6.33, 9] holds 9.
    assert_eq!(counts, [6, 3, 1]);
    assert_eq!(counts.iter().sum::<u64>(), sample.len() as u64);
    let bars = titled(&doc, "rect", "v: ");
    assert_eq!(bars.len(), 3);
    for (b, c) in bars.iter().zip(&counts) {
        let (_, y, _, h) = rect_of(*b);
        assert!((h - (fy.px(0.0) - fy.px(*c as f64))).abs() < 0.02);
        assert!((y + h - fy.px(0.0)).abs() < 0.02);
    }
}

#[test]
fn bins_share_edges_so_their_bars_touch_and_fill_the_plot_width() {
    let r = ok(&with_rows(
        "histogram",
        r#""bins":4"#,
        &["v"],
        &values(&[0, 1, 2, 3, 4, 5, 6, 7, 8]),
    ));
    let doc = parse(&r.svg);
    let bars: Vec<_> = titled(&doc, "rect", "v: ")
        .iter()
        .map(|b| rect_of(*b))
        .collect();
    assert_eq!(bars.len(), 4);
    for w in bars.windows(2) {
        assert!((w[0].0 + w[0].2 - w[1].0).abs() < 0.02, "bars touch");
    }
    assert!(
        (bars[0].0 - r.plot.x).abs() < 0.02
            && (bars[3].0 + bars[3].2 - r.plot.right()).abs() < 0.02
    );
}

#[test]
fn explicit_bins_give_equal_widths_and_auto_bins_give_round_edges() {
    let sample: Vec<i64> = (0..100).map(|i| (i * 37) % 101).collect();
    let r = ok(&with_rows("histogram", "", &["v"], &values(&sample)));
    let edges: Vec<f64> = serde_json::from_value(r.series[0].extra["edges"].clone()).unwrap();
    let step = edges[1] - edges[0];
    assert!(
        edges
            .windows(2)
            .all(|w| ((w[1] - w[0]) - step).abs() < 1e-9)
    );
    assert!(edges[0] <= 0.0 && *edges.last().unwrap() >= 100.0);
    assert!([1.0, 2.0, 5.0, 10.0, 20.0, 50.0].contains(&step), "{step}");
    let counts: Vec<u64> = serde_json::from_value(r.series[0].extra["counts"].clone()).unwrap();
    assert_eq!(counts.iter().sum::<u64>(), 100);
}

#[test]
fn a_series_column_overlays_one_translucent_histogram_per_value_on_shared_bins() {
    let json = r#"{"chart":"histogram","x":"v","series":"g","bins":4,"data":{"columns":["v","g"],"rows":[[1,"a"],[2,"a"],[8,"a"],[5,"b"],[6,"b"],[9,"b"]]}}"#;
    let r = ok(json);
    assert_eq!(r.series.len(), 2);
    assert_eq!(r.series[0].extra["edges"], r.series[1].extra["edges"]);
    let doc = parse(&r.svg);
    assert!(
        all(&doc, "rect")
            .iter()
            .any(|n| n.attribute("fill-opacity") == Some("0.55"))
    );
    let total: u64 = r
        .series
        .iter()
        .map(|s| {
            serde_json::from_value::<Vec<u64>>(s.extra["counts"].clone())
                .unwrap()
                .iter()
                .sum::<u64>()
        })
        .sum();
    assert_eq!(total, 6);
}

#[test]
fn empty_bins_draw_nothing_and_the_y_axis_is_titled_count() {
    let r = ok(&with_rows(
        "histogram",
        r#""bins":5"#,
        &["v"],
        &values(&[0, 0, 10, 10]),
    ));
    let doc = parse(&r.svg);
    assert_eq!(
        titled(&doc, "rect", "v: ").len(),
        2,
        "only the first and last bins hold values"
    );
    assert!(texts(&doc).contains(&"Count".to_owned()));
}

#[test]
fn one_distinct_value_still_makes_a_histogram() {
    let r = ok(&with_rows("histogram", "", &["v"], &values(&[5, 5, 5])));
    let counts: Vec<u64> = serde_json::from_value(r.series[0].extra["counts"].clone()).unwrap();
    assert_eq!(counts.iter().sum::<u64>(), 3);
}

#[test]
fn a_histogram_needs_numbers_and_has_a_linear_count_axis() {
    assert!(
        err(&with_rows(
            "histogram",
            "",
            &["v"],
            &[json!(["a"]), json!(["b"])]
        ))
        .contains("no column of numbers to count")
    );
    assert!(
        err(&with_rows(
            "histogram",
            r#""y_scale":"log""#,
            &["v"],
            &values(&[1, 2])
        ))
        .contains("counts values")
    );
    let r = ok(&with_rows(
        "histogram",
        "",
        &["v", "w"],
        &[json!([1, "x"]), json!([2, "y"])],
    ));
    assert_eq!(r.series[0].name, "v", "the first numeric column by default");
}

#[test]
fn non_numeric_cells_are_reported_and_left_out_of_the_counts() {
    let r = ok(&with_rows(
        "histogram",
        "",
        &["v"],
        &[json!([1]), json!(["oops"]), json!([3])],
    ));
    assert_eq!(r.series[0].points, 2);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("not numbers") && w.contains("\"oops\"")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn the_description_names_the_tallest_bin() {
    let r = ok(&with_rows(
        "histogram",
        r#""bins":2"#,
        &["v"],
        &values(&[0, 0, 0, 10]),
    ));
    assert!(
        r.alt.contains("4 values of v in 2 bins from 0 to 10")
            && r.alt.contains("tallest bin 0 to 5 holds 3"),
        "{}",
        r.alt
    );
}

fn boxes() -> String {
    with_rows(
        "box",
        r#""x":"g","y":["v"]"#,
        &["g", "v"],
        &[1, 2, 3, 4, 5, 6, 7, 8, 9, 40]
            .iter()
            .map(|v| json!(["a", v]))
            .chain([20, 21, 22].iter().map(|v| json!(["b", v])))
            .collect::<Vec<_>>(),
    )
}

#[test]
fn the_box_spans_the_quartiles_with_the_median_line_and_whiskers_inside_the_fences() {
    let r = ok(&boxes());
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    // Group a: 1..9 and 40. q1 = 3.25, median = 5.5, q3 = 7.75, iqr = 4.5, upper fence 14.5: whisker 9, outlier 40.
    let b = titled(&doc, "rect", "a: ")[0];
    let (_, y, _, h) = rect_of(b);
    assert!((y - fy.px(7.75)).abs() < 0.02 && (y + h - fy.px(3.25)).abs() < 0.02);
    let median = all(&doc, "line").into_iter().find(|l| {
        l.attribute("stroke-width") == Some("2") && (num(*l, "y1") - fy.px(5.5)).abs() < 0.02
    });
    assert!(median.is_some(), "the median line is at 5.5");
    let outliers = titled(&doc, "circle", "a: outlier");
    assert_eq!(outliers.len(), 1);
    assert!((num(outliers[0], "cy") - fy.px(40.0)).abs() < 0.02);
    let s = &r.series[0];
    assert_eq!(
        (
            s.extra["q1"].clone(),
            s.extra["median"].clone(),
            s.extra["q3"].clone()
        ),
        (json!(3.25), json!(5.5), json!(7.75))
    );
    assert_eq!(
        (
            s.extra["whisker_low"].clone(),
            s.extra["whisker_high"].clone()
        ),
        (json!(1.0), json!(9.0))
    );
    assert_eq!((s.extra["outlier_count"].clone(), s.points), (json!(1), 10));
    assert_eq!((s.min, s.max), (Some(1.0), Some(40.0)));
}

#[test]
fn whiskers_end_at_data_points_with_end_caps() {
    let r = ok(&boxes());
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let caps: Vec<f64> = all(&doc, "line")
        .into_iter()
        .filter(|l| l.attribute("stroke") == Some("#0072b2") && num(*l, "y1") == num(*l, "y2"))
        .map(|l| num(l, "y1"))
        .collect();
    assert!(
        caps.iter().any(|y| (y - fy.px(1.0)).abs() < 0.02),
        "lower cap at the lowest value inside the fence"
    );
    assert!(
        caps.iter().any(|y| (y - fy.px(9.0)).abs() < 0.02),
        "upper cap at 9, not at the fence or the outlier"
    );
}

#[test]
fn each_group_gets_a_box_in_data_order_and_sorting_applies() {
    let r = ok(&boxes());
    assert_eq!(
        r.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    for sort in ["value", "x_desc"] {
        let sorted = ok(&boxes().replace(
            r#""chart":"box""#,
            &format!(r#""chart":"box","sort":"{sort}""#),
        ));
        assert_eq!(
            sorted
                .series
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"],
            "{sort}"
        );
    }
}

#[test]
fn without_x_every_value_column_is_a_box() {
    let rows = [json!([1, 10]), json!([2, 20]), json!([3, 30])];
    let r = ok(&with_rows("box", "", &["a", "b"], &rows));
    assert_eq!(
        r.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(r.series[1].extra["median"], json!(20.0));
    assert!(texts(&parse(&r.svg)).contains(&"a".to_owned()));
}

#[test]
fn a_group_with_no_numbers_is_left_empty_and_named_in_a_warning() {
    let rows = [
        json!(["a", 1]),
        json!(["a", 2]),
        json!(["b", null]),
        json!(["b", "n/a?"]),
    ];
    let r = ok(&with_rows(
        "box",
        r#""x":"g","y":["v"]"#,
        &["g", "v"],
        &rows,
    ));
    assert_eq!(r.series.len(), 1);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("no numbers for \"b\"")),
        "{:?}",
        r.warnings
    );
    assert!(
        err(&with_rows(
            "box",
            r#""x":"g","y":["v"]"#,
            &["g", "v"],
            &[json!(["a", null])]
        ))
        .contains("no numbers to summarise")
    );
}

#[test]
fn a_log_box_plot_refuses_non_positive_values() {
    let e = err(&with_rows(
        "box",
        r#""y_scale":"log""#,
        &["v"],
        &[json!([1]), json!([0])],
    ));
    assert!(e.contains("logarithmic"), "{e}");
}

#[test]
fn more_outliers_than_can_be_drawn_are_counted_and_the_warning_says_so() {
    let mut rows: Vec<_> = (0..1000).map(|_| json!([10])).collect();
    rows.extend((0..150).map(|i| json!([1000 + i])));
    let r = ok(&with_rows("box", "", &["v"], &rows));
    assert_eq!(titled(&parse(&r.svg), "circle", "v: outlier").len(), 100);
    assert_eq!(r.series[0].extra["outlier_count"], json!(150));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("50 outliers beyond the first 100")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn the_box_description_gives_medians_quartiles_and_outlier_counts() {
    let r = ok(&boxes());
    assert!(
        r.alt
            .contains("\"a\": median 5.5, quartiles 3.25 to 7.75, range 1 to 40, 1 outlier."),
        "{}",
        r.alt
    );
    assert!(
        r.alt.contains("\"b\": median 21") && r.alt.contains("0 outliers"),
        "{}",
        r.alt
    );
}

#[test]
fn rows_without_a_group_label_are_counted_in_warnings() {
    let rows = [
        json!(["a", 1]),
        json!([null, 2]),
        json!([null, 3]),
        json!(["b", 4]),
    ];
    for (chart, extra) in [
        ("histogram", r#""x":"v","series":"g""#),
        ("box", r#""x":"g","y":["v"]"#),
    ] {
        let r = ok(&with_rows(chart, extra, &["g", "v"], &rows));
        assert!(
            r.warnings
                .iter()
                .any(|w| w == "2 rows with a value have no \"g\" label and are left out"),
            "{chart}: {:?}",
            r.warnings
        );
    }
    let rows = [
        json!(["a", "x", 1]),
        json!([null, "y", 2]),
        json!(["b", "y", 3]),
    ];
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["c", "r", "v"],
        &rows,
    ));
    assert!(
        r.warnings
            .iter()
            .any(|w| w == "1 rows with a value have no \"c\" label and are left out"),
        "{:?}",
        r.warnings
    );
}
