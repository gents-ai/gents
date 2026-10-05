//! Bar and combo charts, read back from their SVG.

use serde_json::json;

use crate::testkit::*;

fn bar_rects<'a, 'i>(
    doc: &'a roxmltree::Document<'i>,
    series: &str,
) -> Vec<roxmltree::Node<'a, 'i>> {
    titled(doc, "rect", &format!("{series}, "))
}

fn sales() -> String {
    with_rows(
        "bar",
        "",
        &["region", "q1", "q2"],
        &[
            json!(["North", 10, 20]),
            json!(["South", 30, 5]),
            json!(["East", 20, 15]),
        ],
    )
}

#[test]
fn bar_heights_are_proportional_to_values_and_start_at_the_zero_line() {
    let r = ok(&sales());
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    for (series, values) in [("q1", [10.0, 30.0, 20.0]), ("q2", [20.0, 5.0, 15.0])] {
        let bars = bar_rects(&doc, series);
        assert_eq!(bars.len(), 3);
        for (b, v) in bars.iter().zip(values) {
            let (_, y, _, h) = rect_of(*b);
            assert!(
                (h - (fy.px(0.0) - fy.px(v))).abs() < 0.02,
                "{series} {v}: {h}"
            );
            assert!(
                (y + h - fy.px(0.0)).abs() < 0.02,
                "the bar stands on the zero line"
            );
        }
    }
}

#[test]
fn the_value_axis_starts_at_zero_even_when_every_value_is_large() {
    let r = ok(&with_rows(
        "bar",
        "",
        &["k", "v"],
        &[json!(["a", 1000]), json!(["b", 1010])],
    ));
    let fy = y_fit(&parse(&r.svg), &r.plot);
    assert!((fy.px(0.0) - r.plot.bottom()).abs() < 0.02);
}

#[test]
fn grouped_bars_sit_side_by_side_in_equal_widths_centred_on_the_category() {
    let r = ok(&sales());
    let doc = parse(&r.svg);
    let (q1, q2) = (bar_rects(&doc, "q1"), bar_rects(&doc, "q2"));
    for (a, b) in q1.iter().zip(&q2) {
        let (ax, _, aw, _) = rect_of(*a);
        let (bx, _, bw, _) = rect_of(*b);
        assert!((aw - bw).abs() < 0.02);
        assert!(
            bx >= ax + aw - 0.02 && bx - (ax + aw) < 3.0,
            "neighbours, not overlapping: {ax} {aw} {bx}"
        );
    }
    let labels: Vec<(String, f64)> = all(&doc, "text")
        .into_iter()
        .filter(|n| ["North", "South", "East"].contains(&n.text().unwrap_or("")))
        .map(|n| (n.text().unwrap().to_owned(), num(n, "x")))
        .collect();
    for ((a, b), (name, cx)) in q1.iter().zip(&q2).zip(&labels) {
        let (ax, _, _, _) = rect_of(*a);
        let (bx, _, bw, _) = rect_of(*b);
        assert!(
            ((ax + bx + bw) / 2.0 - cx).abs() < 0.02,
            "{name} is centred under its group"
        );
    }
}

#[test]
fn stacked_bars_stand_on_each_other_and_the_top_is_the_sum() {
    let json = sales().replace(r#""chart":"bar""#, r#""chart":"bar","stack":"stacked""#);
    let r = ok(&json);
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let (q1, q2) = (bar_rects(&doc, "q1"), bar_rects(&doc, "q2"));
    for (i, (a, b)) in q1.iter().zip(&q2).enumerate() {
        let (ax, ay, aw, ah) = rect_of(*a);
        let (bx, by, bw, bh) = rect_of(*b);
        assert!(
            (ax - bx).abs() < 0.02 && (aw - bw).abs() < 0.02,
            "one column"
        );
        assert!(
            (ay - (by + bh)).abs() < 0.02,
            "the second bar stands on the first"
        );
        let sum = [30.0, 35.0, 35.0][i];
        assert!((by - fy.px(sum)).abs() < 0.02, "top of column {i}");
        assert!((ah - (fy.px(0.0) - fy.px([10.0, 30.0, 20.0][i]))).abs() < 0.02);
    }
}

#[test]
fn stacked_negatives_go_below_zero_separately_from_positives() {
    let rows = [json!(["a", 5, -3]), json!(["b", -2, 4])];
    let r = ok(&with_rows("stacked_bar", "", &["k", "p", "q"], &rows));
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let zero = fy.px(0.0);
    let (p, q) = (bar_rects(&doc, "p"), bar_rects(&doc, "q"));
    let (_, py, _, ph) = rect_of(p[0]);
    assert!((py + ph - zero).abs() < 0.02, "5 stands on zero");
    let (_, qy, _, _) = rect_of(q[0]);
    assert!((qy - zero).abs() < 0.02, "-3 hangs from zero");
    let (_, p1y, _, _) = rect_of(p[1]);
    assert!((p1y - zero).abs() < 0.02, "-2 hangs from zero");
    let (_, q1y, _, q1h) = rect_of(q[1]);
    assert!((q1y + q1h - zero).abs() < 0.02, "4 stands on zero");
}

#[test]
fn negative_values_hang_below_the_zero_line() {
    let r = ok(&with_rows(
        "bar",
        "",
        &["k", "v"],
        &[json!(["a", 4]), json!(["b", -6])],
    ));
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let bars = bar_rects(&doc, "v");
    let (_, y0, _, h0) = rect_of(bars[0]);
    let (_, y1, _, h1) = rect_of(bars[1]);
    assert!((y0 + h0 - fy.px(0.0)).abs() < 0.02);
    assert!((y1 - fy.px(0.0)).abs() < 0.02 && (h1 - (fy.px(-6.0) - fy.px(0.0))).abs() < 0.02);
}

#[test]
fn horizontal_bars_grow_rightwards_from_the_zero_line_in_data_order_from_the_top() {
    let r = ok(&with_rows(
        "horizontal_bar",
        "",
        &["k", "v"],
        &[
            json!(["first", 10]),
            json!(["second", 40]),
            json!(["third", 20]),
        ],
    ));
    let doc = parse(&r.svg);
    let fx = x_fit(&doc, &r.plot);
    let bars = bar_rects(&doc, "v");
    let rects: Vec<_> = bars.iter().map(|b| rect_of(*b)).collect();
    for ((x, _, w, _), v) in rects.iter().zip([10.0, 40.0, 20.0]) {
        assert!((x - fx.px(0.0)).abs() < 0.02, "starts at zero");
        assert!((w - (fx.px(v) - fx.px(0.0))).abs() < 0.02, "width of {v}");
    }
    assert!(
        rects[0].1 < rects[1].1 && rects[1].1 < rects[2].1,
        "first category on top"
    );
    let label_ys: Vec<f64> = all(&doc, "text")
        .into_iter()
        .filter(|n| ["first", "second", "third"].contains(&n.text().unwrap_or("")))
        .map(|n| num(n, "y"))
        .collect();
    assert!(label_ys.windows(2).all(|w| w[0] < w[1]));
    assert!(r.alt.starts_with("Horizontal bar chart"), "{}", r.alt);
}

#[test]
fn sorting_orders_the_categories_by_total_or_label() {
    let rows = [json!(["b", 2]), json!(["c", 9]), json!(["a", 5])];
    let order = |sort: &str| {
        let r = ok(&with_rows(
            "bar",
            &format!(r#""sort":"{sort}""#),
            &["k", "v"],
            &rows,
        ));
        let doc = parse(&r.svg);
        let mut labels: Vec<(f64, String)> = all(&doc, "text")
            .into_iter()
            .filter(|n| {
                ["a", "b", "c"].contains(&n.text().unwrap_or(""))
                    && n.attribute("text-anchor") == Some("middle")
            })
            .map(|n| (num(n, "x"), n.text().unwrap().to_owned()))
            .collect();
        labels.sort_by(|a, b| a.0.total_cmp(&b.0));
        labels.into_iter().map(|l| l.1).collect::<Vec<_>>().join("")
    };
    assert_eq!(order("none"), "bca");
    assert_eq!(order("x"), "abc");
    assert_eq!(order("x_desc"), "cba");
    assert_eq!(order("value"), "bac");
    assert_eq!(order("value_desc"), "cab");
}

#[test]
fn a_missing_value_leaves_a_gap_not_a_zero_bar() {
    let rows = [json!(["a", 5, null]), json!(["b", null, 7])];
    let r = ok(&with_rows("bar", "", &["k", "p", "q"], &rows));
    let doc = parse(&r.svg);
    assert_eq!(bar_rects(&doc, "p").len(), 1);
    assert_eq!(bar_rects(&doc, "q").len(), 1);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("empty values are drawn as gaps, not zeros"))
    );
    assert_eq!((r.series[0].points, r.series[0].gaps), (1, 1));
}

#[test]
fn rows_with_the_same_category_add_up_or_use_the_chosen_aggregate() {
    let rows = [json!(["a", 1]), json!(["a", 3]), json!(["b", 4])];
    let sum = ok(&with_rows("bar", "", &["k", "v"], &rows));
    assert_eq!(sum.series[0].extra["sum"], json!(8.0));
    assert_eq!(
        (sum.series[0].min, sum.series[0].max),
        (Some(4.0), Some(4.0))
    );
    let mean = ok(&with_rows("bar", r#""agg":"mean""#, &["k", "v"], &rows));
    assert_eq!(
        (mean.series[0].min, mean.series[0].max),
        (Some(2.0), Some(4.0))
    );
    let count = ok(&with_rows("bar", r#""agg":"count""#, &["k", "v"], &rows));
    assert_eq!(
        (count.series[0].min, count.series[0].max),
        (Some(1.0), Some(2.0))
    );
    let max = ok(&with_rows("bar", r#""agg":"max""#, &["k", "v"], &rows));
    assert_eq!(max.series[0].max, Some(4.0));
}

#[test]
fn value_labels_are_drawn_on_few_bars_and_left_off_stacks_and_crowds() {
    let r = ok(&with_rows(
        "bar",
        "",
        &["k", "v"],
        &[json!(["a", 12]), json!(["b", 7])],
    ));
    let t = texts(&parse(&r.svg));
    assert!(t.contains(&"12".to_owned()) && t.contains(&"7".to_owned()));
    let stacked = ok(&with_rows(
        "stacked_bar",
        "",
        &["k", "p", "q"],
        &[json!(["a", 11, 7]), json!(["b", 13, 3])],
    ));
    let t = texts(&parse(&stacked.svg));
    for v in ["11", "7", "13", "3"] {
        assert!(
            !t.contains(&v.to_owned()),
            "no per-bar labels on a stack: {t:?}"
        );
    }
    let rows: Vec<_> = (0..40).map(|i| json!([format!("c{i}"), i + 1])).collect();
    let crowd = ok(&with_rows("bar", "", &["k", "v"], &rows));
    let t = texts(&parse(&crowd.svg));
    assert!(!t.contains(&"37".to_owned()), "no value labels on 40 bars");
}

#[test]
fn a_user_format_applies_to_value_labels_and_the_axis() {
    let r = ok(&with_rows(
        "bar",
        r#""format":"${,.0f}""#,
        &["k", "v"],
        &[json!(["a", 1234]), json!(["b", 987])],
    ));
    let t = texts(&parse(&r.svg));
    assert!(
        t.contains(&"$1,234".to_owned()) && t.contains(&"$987".to_owned()),
        "{t:?}"
    );
    assert!(t.contains(&"$0".to_owned()), "axis ticks use it too");
}

#[test]
fn long_category_labels_rotate_and_carry_their_full_text_in_a_tooltip() {
    let long = "Operations and maintenance of regional facilities";
    let rows: Vec<_> = (0..12)
        .map(|i| json!([format!("{long} {i}"), i + 1]))
        .collect();
    let r = ok(&with_rows("bar", "", &["k", "v"], &rows));
    let doc = parse(&r.svg);
    let rotated: Vec<_> = all(&doc, "text")
        .into_iter()
        .filter(|n| {
            n.attribute("transform")
                .is_some_and(|t| t.starts_with("rotate(-45"))
        })
        .collect();
    assert!(
        !rotated.is_empty(),
        "labels rotate when they do not fit flat"
    );
    let shortened: Vec<_> = rotated
        .iter()
        .filter(|n| n.text().is_some_and(|t| t.ends_with('\u{2026}')))
        .collect();
    assert!(!shortened.is_empty());
    for n in shortened {
        let g = n.parent().unwrap();
        let full = title(g).expect("a tooltip with the full label");
        assert!(full.starts_with(long) && full.len() > n.text().unwrap().len());
    }
}

#[test]
fn crowded_labels_are_thinned_and_the_warning_says_how() {
    let rows: Vec<_> = (0..90)
        .map(|i| json!([format!("category {i}"), i + 1]))
        .collect();
    let r = ok(&with_rows("bar", "", &["k", "v"], &rows));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("only every") && w.contains("of 90 category labels")),
        "{:?}",
        r.warnings
    );
    assert_eq!(
        bar_rects(&parse(&r.svg), "v").len(),
        90,
        "every bar is still drawn"
    );
}

#[test]
fn more_than_a_hundred_categories_are_capped_with_a_warning() {
    let rows: Vec<_> = (0..130).map(|i| json!([format!("c{i:03}"), 1])).collect();
    let r = ok(&with_rows("bar", "", &["k", "v"], &rows));
    assert_eq!(bar_rects(&parse(&r.svg), "v").len(), 100);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("130 categories; only the first 100 in data order")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_log_axis_starts_bars_at_the_bottom_and_refuses_zero() {
    let r = ok(&with_rows(
        "bar",
        r#""y_scale":"log""#,
        &["k", "v"],
        &[json!(["a", 10]), json!(["b", 1000])],
    ));
    let doc = parse(&r.svg);
    for b in bar_rects(&doc, "v") {
        let (_, y, _, h) = rect_of(b);
        assert!(
            (y + h - r.plot.bottom()).abs() < 0.05,
            "stands on the bottom of the plot"
        );
    }
    let e = err(&with_rows(
        "bar",
        r#""y_scale":"log""#,
        &["k", "v"],
        &[json!(["a", 0])],
    ));
    assert!(e.contains("logarithmic"), "{e}");
    let e = err(&sales().replace(
        r#""chart":"bar""#,
        r#""chart":"bar","y_scale":"log","stack":"stacked""#,
    ));
    assert!(e.contains("stacked bars need a linear y axis"), "{e}");
}

#[test]
fn a_stack_of_one_series_says_it_has_no_effect() {
    let r = ok(&with_rows(
        "bar",
        r#""stack":"stacked""#,
        &["k", "v"],
        &[json!(["a", 1])],
    ));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("stacked has no effect with one series"))
    );
}

#[test]
fn legend_swatches_match_the_bar_colours() {
    let r = ok(&sales());
    let doc = parse(&r.svg);
    let swatches: Vec<String> = all(&doc, "rect")
        .into_iter()
        .filter(|n| num(*n, "width") == 12.0 && num(*n, "height") == 12.0 && title(*n).is_none())
        .filter_map(|n| n.attribute("fill").map(str::to_owned))
        .collect();
    assert_eq!(swatches, ["#0072b2", "#e69f00"]);
}

fn combo() -> String {
    with_rows(
        "combo",
        r#""y":["revenue"],"line":["margin"]"#,
        &["m", "revenue", "margin"],
        &[
            json!(["Jan", 100, 0.2]),
            json!(["Feb", 150, 0.3]),
            json!(["Mar", 120, 0.25]),
        ],
    )
}

#[test]
fn a_combo_draws_bars_on_the_left_axis_and_a_line_on_the_right_axis() {
    let r = ok(&combo());
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    let fy2 = y2_fit(&doc, &r.plot);
    for (b, v) in bar_rects(&doc, "revenue").iter().zip([100.0, 150.0, 120.0]) {
        let (_, y, _, _) = rect_of(*b);
        assert!((y - fy.px(v)).abs() < 0.02);
    }
    let line = &stroked_paths(&doc, "#e69f00")[0];
    for (p, v) in line.iter().zip([0.2, 0.3, 0.25]) {
        assert!((p.1 - fy2.px(v)).abs() < 0.02, "{} vs {}", p.1, fy2.px(v));
    }
    assert_eq!(
        r.series
            .iter()
            .map(|s| (s.mark, s.axis))
            .collect::<Vec<_>>(),
        [("bar", "left"), ("line", "right")]
    );
    assert!(
        r.alt.starts_with("Combined bar and line chart"),
        "{}",
        r.alt
    );
    assert!(r.alt.contains("Right axis: margin"), "{}", r.alt);
}

#[test]
fn line_points_sit_over_the_centre_of_their_category() {
    let r = ok(&combo());
    let doc = parse(&r.svg);
    let line = &stroked_paths(&doc, "#e69f00")[0];
    let bars = bar_rects(&doc, "revenue");
    for (p, b) in line.iter().zip(&bars) {
        let (x, _, w, _) = rect_of(*b);
        assert!((p.0 - (x + w / 2.0)).abs() < 0.02);
    }
}

#[test]
fn the_right_axis_can_have_its_own_format_and_log_scale() {
    let r = ok(&combo().replace(
        r#""chart":"combo""#,
        r#""chart":"combo","y2_format":"{.0%}","y2_label":"Margin""#,
    ));
    let t = texts(&parse(&r.svg));
    assert!(t.iter().any(|l| l.ends_with('%')), "{t:?}");
    assert!(t.contains(&"Margin".to_owned()));
    let rows = [json!(["a", 5, 10]), json!(["b", 6, 1000])];
    let r = ok(&with_rows(
        "combo",
        r#""y":["v"],"line":["w"],"y2_scale":"log""#,
        &["k", "v", "w"],
        &rows,
    ));
    assert_eq!(r.series[1].axis, "right");
}

#[test]
fn the_lines_can_use_the_left_axis() {
    let r = ok(&combo().replace(
        r#""chart":"combo""#,
        r#""chart":"combo","line_axis":"left""#,
    ));
    let doc = parse(&r.svg);
    let fy = y_fit(&doc, &r.plot);
    assert_eq!(r.series[1].axis, "left");
    let line = &stroked_paths(&doc, "#e69f00")[0];
    assert!((line[0].1 - fy.px(0.2)).abs() < 0.02);
}

#[test]
fn a_combo_needs_line_columns_and_no_series_column() {
    let e = err(&with_rows("combo", "", &["k", "v"], &[json!(["a", 1])]));
    assert!(e.contains("needs line columns"), "{e}");
    let json = r#"{"chart":"combo","line":["w"],"series":"s","data":{"columns":["k","s","v","w"],"rows":[["a","x",1,2]]}}"#;
    assert!(err(json).contains("not a series column"));
}

#[test]
fn bar_columns_default_to_the_numeric_columns_that_are_not_lines() {
    let rows = [json!(["a", 5, 10, 0.5]), json!(["b", 6, 12, 0.6])];
    let r = ok(&with_rows(
        "combo",
        r#""line":["rate"]"#,
        &["k", "v", "w", "rate"],
        &rows,
    ));
    assert_eq!(
        r.series
            .iter()
            .map(|s| (s.name.as_str(), s.mark))
            .collect::<Vec<_>>(),
        [("v", "bar"), ("w", "bar"), ("rate", "line")]
    );
}

#[test]
fn horizontal_is_ignored_for_combos_with_a_note() {
    let r = ok(&combo().replace(r#""chart":"combo""#, r#""chart":"combo","horizontal":true"#));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("horizontal only applies to bar charts")),
        "{:?}",
        r.warnings
    );
    let doc = parse(&r.svg);
    let bars = bar_rects(&doc, "revenue");
    let (_, _, w, h) = rect_of(bars[0]);
    assert!(h > w, "still vertical bars");
}
