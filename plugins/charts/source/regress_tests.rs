//! Regression tests for review defects and killers for mutants the first
//! suites missed. Each test names the behaviour it pins.

use serde_json::{Value, json};

use crate::testkit::*;

/// Every `<text>` that reads as a year, in document order.
fn year_labels(svg: &str) -> Vec<String> {
    let doc = parse(svg);
    texts(&doc)
        .into_iter()
        .filter(|t| {
            t.len() == 4 && t.starts_with(['1', '2']) && t.bytes().all(|b| b.is_ascii_digit())
        })
        .collect()
}

fn year_rows(first: i64, n: i64) -> Vec<Value> {
    (0..n).map(|i| json!([first + i, 10 + i * 3])).collect()
}

#[test]
fn year_axes_tick_on_whole_years_for_short_spans() {
    for (first, n, expect) in [
        (2019, 3, vec!["2019", "2020", "2021"]),
        (2015, 5, vec!["2015", "2016", "2017", "2018", "2019"]),
        (2020, 2, vec!["2020", "2021"]),
    ] {
        for chart in ["line", "area", "scatter"] {
            let rows = year_rows(first, n);
            let r = ok(&with_rows(
                chart,
                r#""output":"svg""#,
                &["year", "v"],
                &rows,
            ));
            let svg = &r.svg;
            let labels = year_labels(svg);
            if chart == "scatter" {
                // A scatter axis is padded past the data; its ticks stay whole years.
                assert!(labels.windows(2).all(|w| w[0] < w[1]), "{labels:?}");
                assert!(
                    expect.iter().all(|y| labels.iter().any(|l| l == y)),
                    "{labels:?}"
                );
            } else {
                assert_eq!(labels, expect, "{chart} {first}+{n}");
            }
        }
    }
}

#[test]
fn a_whole_number_axis_never_repeats_a_tick_label() {
    for n in 2..=14 {
        let rows = year_rows(2001, n);
        let r = ok(&with_rows(
            "line",
            r#""output":"svg""#,
            &["year", "v"],
            &rows,
        ));
        let labels = year_labels(&r.svg);
        let mut sorted = labels.clone();
        sorted.dedup();
        assert_eq!(labels, sorted, "n={n}");
        assert!(labels.windows(2).all(|w| w[0] < w[1]), "n={n}: {labels:?}");
    }
}

fn mixed_labels(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| match i % 6 {
            0 => format!("{}", (i * 7) % 113),
            1 => format!("{}a", (i * 3) % 29),
            2 => format!("b{}", (i * 5) % 31),
            3 => format!("2024-{:02}-{:02}", i % 12 + 1, i % 28 + 1),
            4 => format!("{}", ((i * 11) % 97) as f64 / 4.0),
            _ => format!("{}x", i % 17),
        })
        .collect()
}

#[test]
fn sorting_mixed_labels_is_total_and_never_panics() {
    use crate::cols::order;
    use crate::spec::Sort;
    let names = mixed_labels(240);
    let totals = vec![0.0; names.len()];
    for sort in [Sort::X, Sort::XDesc] {
        let idx = order(&names, &totals, sort);
        let mut seen = idx.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..names.len()).collect::<Vec<_>>(), "a permutation");
        // Numbers come first, then dates, then text, whichever way it sorts.
        let class = |s: &str| {
            if s.parse::<f64>().is_ok() {
                0
            } else if s.starts_with("2024-") {
                1
            } else {
                2
            }
        };
        let classes: Vec<u8> = idx.iter().map(|i| class(&names[*i])).collect();
        if sort == Sort::X {
            assert!(classes.windows(2).all(|w| w[0] <= w[1]), "{classes:?}");
        } else {
            assert!(classes.windows(2).all(|w| w[0] >= w[1]), "{classes:?}");
        }
    }
}

#[test]
fn numeric_labels_sort_by_value_and_text_by_bytes() {
    use crate::cols::order;
    use crate::spec::Sort;
    let names: Vec<String> = ["10", "2", "1a", "b", "-3", "2024-01-05", "a", "1.5"]
        .map(String::from)
        .to_vec();
    let totals = vec![0.0; names.len()];
    let by = |sort| -> Vec<&str> {
        order(&names, &totals, sort)
            .into_iter()
            .map(|i| names[i].as_str())
            .collect()
    };
    assert_eq!(
        by(Sort::X),
        ["-3", "1.5", "2", "10", "2024-01-05", "1a", "a", "b"]
    );
    assert_eq!(
        by(Sort::XDesc),
        ["b", "a", "1a", "2024-01-05", "10", "2", "1.5", "-3"]
    );
}

#[test]
fn repeated_and_empty_header_names_are_made_unique() {
    let h = |v: &[&str]| {
        crate::table::normalize_header(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    };
    assert_eq!(
        h(&["a", "a", "a", "a_2", ""]),
        ["a", "a_2", "a_3", "a_2_2", "column 5"]
    );
    assert_eq!(h(&["a", "a_2", "a"]), ["a", "a_2", "a_3"]);
    assert_eq!(h(&[" x ", "x"]), ["x", "x_2"]);
}

#[test]
fn a_header_with_a_hundred_thousand_columns_is_read_in_bounded_time() {
    let start = std::time::Instant::now();
    for name in ["c", "same"] {
        let header: Vec<String> = (0..100_000)
            .map(|i| {
                if name == "c" {
                    format!("c{i}")
                } else {
                    "same".into()
                }
            })
            .collect();
        let names = crate::table::normalize_header(&header);
        assert_eq!(names.len(), 100_000);
    }
    let csv = (0..100_000)
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let ones = vec!["1"; 100_000].join(",");
    let req =
        json!({"chart":"bar","x":"c0","y":["c1"],"data":format!("{csv}\n{ones}\n"),"output":"svg"});
    let out = crate::run(&req.to_string());
    assert!(out.is_ok(), "{out:?}");
    assert!(start.elapsed().as_secs() < 20, "took {:?}", start.elapsed());
}

#[test]
fn quote_keeps_unicode_quotes_and_backslashes_as_written() {
    use crate::text::quote;
    assert_eq!(quote("e\u{301}cole"), "\"e\u{301}cole\"");
    assert_eq!(quote("\u{2764}\u{fe0f}"), "\"\u{2764}\u{fe0f}\"");
    assert_eq!(quote(r#"say "hi" \o/"#), r#""say "hi" \o/""#);
    assert_eq!(quote("a\nb\tc"), "\"a b c\"");
    assert_eq!(quote(""), "\"\"");
}

#[test]
fn descriptions_and_warnings_show_text_without_rust_escapes() {
    let tricky = "e\u{301}cole \u{2764}\u{fe0f} say \"hi\" \\o/";
    let rows = [json!([tricky, 3]), json!(["plain", 5])];
    for chart in ["pie", "bar", "line", "box", "histogram"] {
        let extra = format!(r#""title":{}"#, serde_json::to_string(tricky).unwrap());
        let cols = ["k", "v"];
        let rows: Vec<Value> = if chart == "line" || chart == "histogram" {
            vec![json!([1, 3]), json!([2, 5])]
        } else {
            rows.to_vec()
        };
        let extra = if chart == "line" || chart == "histogram" {
            format!(r#"{extra},"y":["v"],"series":"k""#)
        } else {
            extra
        };
        let json = with_rows(chart, &extra, &cols, &rows);
        let r = match render(&json) {
            Ok((r, _)) => r,
            Err(e) => panic!("{chart}: {e}"),
        };
        for text in std::iter::once(&r.alt).chain(&r.warnings) {
            assert!(!text.contains("\\u{"), "{chart}: {text}");
            assert!(!text.contains("\\\""), "{chart}: {text}");
            assert!(!text.contains("\\\\"), "{chart}: {text}");
        }
        assert!(
            r.alt.contains(&format!("titled \"{tricky}\"")),
            "{chart}: {}",
            r.alt
        );
    }
}

#[test]
fn a_name_in_an_error_keeps_its_accents_and_quotes() {
    let e = err(&with_rows(
        "bar",
        r#""x":"e\u0301cole \"x\"","y":["v"]"#,
        &["k", "v"],
        &[json!(["a", 1])],
    ));
    assert!(e.contains("\"e\u{301}cole \"x\"\""), "{e}");
    assert!(!e.contains("\\u{") && !e.contains("\\\""), "{e}");
}

#[test]
fn slice_labels_and_series_names_are_quoted_as_written_in_the_alt_text() {
    let tricky = "e\u{301}cole \u{2764}\u{fe0f} \"q\"";
    let pie = ok(&with_rows(
        "pie",
        "",
        &["k", "v"],
        &[json!([tricky, 3]), json!(["b", 1])],
    ));
    assert!(
        pie.alt.contains(&format!(" \"{tricky}\" 3 ")),
        "{}",
        pie.alt
    );
    for chart in ["bar", "line", "scatter"] {
        let cols = ["x", tricky];
        let rows = [json!([1, 2]), json!([2, 3]), json!([3, 5])];
        let extra = format!(
            r#""x":"x","y":[{}]"#,
            serde_json::to_string(tricky).unwrap()
        );
        let r = ok(&with_rows(chart, &extra, &cols, &rows));
        assert!(
            r.alt.contains(&format!("\"{tricky}\"")),
            "{chart}: {}",
            r.alt
        );
    }
    let extra = format!(r#""x":{}"#, serde_json::to_string(tricky).unwrap());
    let h = ok(&with_rows(
        "histogram",
        &extra,
        &[tricky],
        &[json!([1]), json!([2]), json!([4])],
    ));
    assert!(
        h.alt.contains(&format!("\"{tricky}\": 3 values")),
        "{}",
        h.alt
    );
}

fn cut_warnings(chart: &str, extra: &str, cols: &[&str], rows: &[Value]) -> Vec<String> {
    ok(&with_rows(chart, extra, cols, rows)).warnings
}

#[test]
fn a_fixed_y_range_reports_the_values_it_cuts_off() {
    let rows = [
        json!(["a", 1]),
        json!(["b", 10]),
        json!(["c", 3]),
        json!(["d", 12]),
    ];
    for chart in ["line", "area", "bar"] {
        let w = cut_warnings(chart, r#""x":"k","y":["v"],"y_max":5"#, &["k", "v"], &rows);
        assert!(
            w.iter()
                .any(|w| w == "series \"v\": 2 values are above y_max and are cut off"),
            "{chart}: {w:?}"
        );
        let w = cut_warnings(chart, r#""x":"k","y":["v"],"y_min":2"#, &["k", "v"], &rows);
        assert!(
            w.iter()
                .any(|w| w == "series \"v\": 1 value is below y_min and is cut off"),
            "{chart}: {w:?}"
        );
        // A bound that cuts nothing warns of nothing, and a value on the bound is kept.
        let w = cut_warnings(
            chart,
            r#""x":"k","y":["v"],"y_min":1,"y_max":12"#,
            &["k", "v"],
            &rows,
        );
        assert!(w.iter().all(|w| !w.contains("cut off")), "{chart}: {w:?}");
    }
}

#[test]
fn stacked_charts_count_the_cut_by_the_stack_top() {
    let rows = [json!(["a", 4, 4]), json!(["b", 1, 1])];
    for chart in ["stacked_area", "stacked_bar"] {
        let w = cut_warnings(
            chart,
            r#""x":"k","y":["p","q"],"y_max":6"#,
            &["k", "p", "q"],
            &rows,
        );
        assert!(
            w.iter()
                .any(|w| w == "series \"q\": 1 value is above y_max and is cut off"),
            "{chart}: {w:?}"
        );
        assert!(w.iter().all(|w| !w.contains("\"p\":")), "{chart}: {w:?}");
    }
}

#[test]
fn a_fixed_x_range_reports_cut_points_and_the_description_gives_the_drawn_range() {
    let rows = [json!([1, 5]), json!([2, 6]), json!([3, 7]), json!([9, 8])];
    for chart in ["line", "scatter"] {
        let r = ok(&with_rows(
            chart,
            r#""x":"a","y":["b"],"x_min":2,"x_max":4"#,
            &["a", "b"],
            &rows,
        ));
        assert!(
            r.warnings
                .iter()
                .any(|w| w == "series \"b\": 1 value is below x_min and is cut off"),
            "{chart}: {:?}",
            r.warnings
        );
        assert!(
            r.warnings
                .iter()
                .any(|w| w == "series \"b\": 1 value is above x_max and is cut off"),
            "{chart}: {:?}",
            r.warnings
        );
        assert!(
            r.alt.contains("X axis: a, from 2 to 4."),
            "{chart}: {}",
            r.alt
        );
    }
}

#[test]
fn a_fixed_y_range_is_in_the_description() {
    let rows = [json!(["a", 1]), json!(["b", 10]), json!(["c", 3])];
    let r = ok(&with_rows(
        "line",
        r#""x":"k","y":["v"],"y_max":5"#,
        &["k", "v"],
        &rows,
    ));
    assert!(
        r.alt.contains("Y axis: v, from ") && r.alt.contains(" to 5."),
        "{}",
        r.alt
    );
}

fn save_of(json: &str) -> Result<(Option<String>, Option<String>), String> {
    let req: crate::spec::Request = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let s = req.resolve().map_err(|e| e.0)?;
    let save = s.save.ok_or("no save")?;
    Ok((save.svg, save.png))
}

#[test]
fn a_save_base_name_that_ends_in_an_image_extension_is_not_doubled() {
    let pair = |a: &str, b: &str| Ok((Some(a.to_owned()), Some(b.to_owned())));
    assert_eq!(
        save_of(r#"{"chart":"line","save":"chart.svg"}"#),
        pair("chart.svg", "chart.png")
    );
    assert_eq!(
        save_of(r#"{"chart":"line","save":"out/chart.PNG"}"#),
        pair("out/chart.svg", "out/chart.png")
    );
    assert_eq!(
        save_of(r#"{"chart":"line","save":"chart"}"#),
        pair("chart.svg", "chart.png")
    );
    assert_eq!(
        save_of(r#"{"chart":"line","save":"a.v2"}"#),
        pair("a.v2.svg", "a.v2.png")
    );
    // Only one extension is removed.
    assert_eq!(
        save_of(r#"{"chart":"line","save":"x.svg.png"}"#),
        pair("x.svg.svg", "x.svg.png")
    );
}

#[test]
fn a_save_error_quotes_the_callers_own_text() {
    for given in ["../x", "../x.svg", "a/../b", "/abs", "a//b.png"] {
        let e = save_of(&format!(r#"{{"chart":"line","save":"{given}"}}"#)).unwrap_err();
        assert!(e.contains(&format!("\"{given}\"")), "{given}: {e}");
        assert!(
            !e.contains(&format!("\"{given}.svg\"")) && !e.contains(&format!("\"{given}.png\"")),
            "{given}: {e}"
        );
    }
    for given in [".svg", ".png", "", "dir/"] {
        let e = save_of(&format!(r#"{{"chart":"line","save":"{given}"}}"#)).unwrap_err();
        assert!(e.contains("save needs a file name"), "{given:?}: {e}");
    }
}

#[test]
fn x_defaults_to_the_row_number_when_the_first_column_is_also_a_value() {
    let rows = [json!([5]), json!([9]), json!([7])];
    for chart in ["line", "area", "scatter", "bar"] {
        let r = ok(&with_rows(chart, r#""y":["v"]"#, &["v"], &rows));
        assert!(r.alt.contains("row"), "{chart}: {}", r.alt);
        assert!(
            chart == "bar" || r.alt.contains("X axis: row"),
            "{chart}: {}",
            r.alt
        );
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("rows are numbered along x")),
            "{chart}: {:?}",
            r.warnings
        );
    }
    let r = ok(&with_rows("line", r#""y":["v"]"#, &["v"], &rows));
    // The line is not the diagonal v against v: the y values 5, 9, 7 keep their order.
    let doc = parse(&r.svg);
    let line = stroked_paths(&doc, &r.series[0].color);
    let pts = &line[0];
    assert_eq!(pts.len(), 3);
    assert!(
        pts[1].1 < pts[0].1 && pts[1].1 < pts[2].1,
        "9 is highest: {pts:?}"
    );
}

#[test]
fn a_named_x_or_another_value_column_keeps_the_normal_default() {
    let rows = [json!([1, 5]), json!([2, 9])];
    let r = ok(&with_rows("line", r#""y":["b"]"#, &["a", "b"], &rows));
    assert!(r.alt.contains("X axis: a"), "{}", r.alt);
    assert!(
        r.warnings.iter().all(|w| !w.contains("numbered")),
        "{:?}",
        r.warnings
    );
    let r = ok(&with_rows(
        "line",
        r#""x":"a","y":["a","b"]"#,
        &["a", "b"],
        &rows,
    ));
    assert!(r.alt.contains("X axis: a"), "{}", r.alt);
}

#[test]
fn a_title_at_the_smallest_documented_size_is_dropped_with_a_warning_not_an_error() {
    let rows = [json!(["a", 1]), json!(["b", 2]), json!(["c", 3])];
    for chart in ["bar", "line", "pie", "histogram", "box"] {
        let extra = r#""title":"T","subtitle":"s","width":200,"height":150,"x":"k","y":["v"]"#;
        let extra = if chart == "histogram" {
            r#""title":"T","width":200,"height":150,"x":"v""#
        } else {
            extra
        };
        let cols = ["k", "v"];
        let rows: Vec<Value> = if chart == "line" {
            vec![json!([1, 1]), json!([2, 2]), json!([3, 3])]
        } else {
            rows.to_vec()
        };
        let r = ok(&with_rows(chart, extra, &cols, &rows));
        assert_eq!((r.width, r.height), (200, 150), "{chart}");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("title") && w.contains("left out")),
            "{chart}: {:?}",
            r.warnings
        );
        // The description still names the title the picture could not show.
        assert!(r.alt.contains("titled \"T\""), "{chart}: {}", r.alt);
    }
}

#[test]
fn a_title_that_fits_is_drawn_and_not_reported_as_dropped() {
    let rows = [json!(["a", 1]), json!(["b", 2])];
    let r = ok(&with_rows(
        "bar",
        r#""title":"T","x":"k","y":["v"]"#,
        &["k", "v"],
        &rows,
    ));
    assert!(texts(&parse(&r.svg)).iter().any(|t| t == "T"));
    assert!(
        r.warnings.iter().all(|w| !w.contains("left out")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_heatmap_at_the_smallest_size_drops_the_colour_bar_with_a_warning() {
    let rows = [
        json!(["x", "p", 1]),
        json!(["y", "q", 2]),
        json!(["z", "p", 3]),
    ];
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"a","y":["b"],"value":"c","width":200,"height":200"#,
        &["a", "b", "c"],
        &rows,
    ));
    assert!(!r.svg.contains("url(#scale)"));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("colour bar is left out")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn na_and_none_are_labels_in_a_text_column_and_gaps_in_a_numeric_one() {
    // A country code and a "None" label are categories, not missing values.
    let rows = [
        json!(["US", 3]),
        json!(["NA", 5]),
        json!(["None", 2]),
        json!(["DE", 4]),
    ];
    let r = ok(&with_rows(
        "bar",
        r#""x":"k","y":["v"]"#,
        &["k", "v"],
        &rows,
    ));
    assert!(r.alt.contains("4 categories"), "{}", r.alt);
    assert!(
        r.warnings.iter().all(|w| !w.contains("no value")),
        "{:?}",
        r.warnings
    );
    let pie = ok(&with_rows("pie", "", &["k", "v"], &rows));
    assert!(
        pie.alt.contains("\"NA\" 5") && pie.alt.contains("\"None\" 2"),
        "{}",
        pie.alt
    );
    // Among numbers the same words are gaps, reported as empty values.
    let nums = [
        json!(["a", 1]),
        json!(["b", "NA"]),
        json!(["c", "N/A"]),
        json!(["d", 4]),
    ];
    let r = ok(&with_rows(
        "line",
        r#""x":"k","y":["v"]"#,
        &["k", "v"],
        &nums,
    ));
    assert_eq!(r.series[0].gaps, 2);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("2 empty values are drawn as gaps")),
        "{:?}",
        r.warnings
    );
    assert!(
        r.warnings.iter().all(|w| !w.contains("not numbers")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_column_of_only_markers_is_text_and_a_marker_among_dates_is_a_gap_in_the_time() {
    use crate::cols::{ColKind, kind, times};
    use crate::table::Builder;
    let col = |cells: &[&str]| {
        let mut b = Builder::new(None);
        b.set_header(&["c".to_owned()]);
        for c in cells {
            let cell = b.text_cell(c);
            b.push_positional(&[cell]);
        }
        b.finish()
    };
    assert_eq!(kind(&col(&["None", "NA"]), 0), ColKind::Text);
    assert_eq!(kind(&col(&["1", "NA", "3"]), 0), ColKind::Numeric);
    let t = col(&["2024-01-01", "NA", "2024-01-03"]);
    assert_eq!(kind(&t, 0), ColKind::Time);
    let v = times(&t, 0).unwrap();
    assert!(v[1].is_nan() && v[0] < v[2]);
}

fn decimal_comma_csv(extra: &str) -> String {
    let csv = "month;sales;share\nJan;1.234,5;12,5\nFeb;-2,75;0,5\nMar;1000;3\n";
    format!(
        r#"{{"chart":"bar","x":"month","y":["sales","share"],"output":"svg"{extra},"data":{}}}"#,
        serde_json::to_string(csv).unwrap()
    )
}

#[test]
fn a_semicolon_file_with_decimal_commas_is_read_as_numbers_and_says_so() {
    let r = ok(&decimal_comma_csv(""));
    assert_eq!(r.series.len(), 2);
    assert_eq!(
        (r.series[0].max, r.series[0].min),
        (Some(1234.5), Some(-2.75))
    );
    assert_eq!((r.series[1].max, r.series[1].min), (Some(12.5), Some(0.5)));
    assert!(
        r.warnings
            .iter()
            .any(|w| w == "4 values written with a decimal comma were read as numbers"),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_narrow_span_far_from_zero_keeps_every_digit_instead_of_repeating_si_labels() {
    use crate::axes::{NumSpec, numeric};
    use crate::frame::AxisKind;
    let a = numeric(
        NumSpec {
            min: -40_621_140.77,
            max: -40_560_789.24,
            user_min: None,
            user_max: None,
            zero: false,
            nice: true,
            log: false,
            integer: false,
        },
        2,
        None,
        None,
    );
    let AxisKind::Cont { ticks, .. } = a.kind else {
        panic!("a continuous axis")
    };
    let labels: Vec<&str> = ticks.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "-40,640,000",
            "-40,620,000",
            "-40,600,000",
            "-40,580,000",
            "-40,560,000"
        ]
    );
    // A wide span keeps the short SI form.
    let wide = numeric(
        NumSpec {
            min: 0.0,
            max: 3e6,
            user_min: None,
            user_max: None,
            zero: false,
            nice: true,
            log: false,
            integer: false,
        },
        4,
        None,
        None,
    );
    let AxisKind::Cont { ticks, .. } = wide.kind else {
        panic!("continuous")
    };
    assert_eq!(ticks.last().map(|t| t.label.as_str()), Some("3M"));
}

#[test]
fn a_single_hidden_or_floored_bubble_reads_in_the_singular() {
    let rows = [json!([1, 1, 0]), json!([2, 2, 1]), json!([3, 3, 1_000_000])];
    let r = ok(&with_rows(
        "bubble",
        r#""x":"a","y":["b"],"size":"s""#,
        &["a", "b", "s"],
        &rows,
    ));
    assert!(
        r.warnings
            .iter()
            .any(|w| w == "series \"b\": 1 bubble with a size of zero or below is not drawn"),
        "{:?}",
        r.warnings
    );
    assert!(
        r.warnings.iter().any(|w| w
            == "series \"b\": 1 bubble too small to show to scale and drawn at the smallest size"),
        "{:?}",
        r.warnings
    );
}

#[test]
fn one_empty_heatmap_cell_reads_in_the_singular() {
    let rows = [
        json!(["a", "x", 1]),
        json!(["a", "y", 2]),
        json!(["b", "x", 3]),
    ];
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &rows,
    ));
    assert!(
        r.warnings
            .iter()
            .any(|w| w == "1 cell has no value and is drawn empty"),
        "{:?}",
        r.warnings
    );
}
