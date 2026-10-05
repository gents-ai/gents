//! Pie, donut and heatmap charts, read back from their SVG.

use serde_json::json;

use crate::palette::{DARK, LIGHT, diverging, hex, sequential};
use crate::testkit::*;

fn shares() -> String {
    with_rows(
        "pie",
        "",
        &["k", "v"],
        &[
            json!(["a", 50]),
            json!(["b", 25]),
            json!(["c", 15]),
            json!(["d", 10]),
        ],
    )
}

/// (start, end) angle of each slice read back from its path, in degrees clockwise from the top.
fn slice_angles(doc: &roxmltree::Document<'_>) -> Vec<(f64, f64)> {
    let paths: Vec<_> = all(doc, "path")
        .into_iter()
        .filter(|p| title(*p).is_some())
        .collect();
    paths
        .iter()
        .map(|p| {
            let pts = path_points(p.attribute("d").unwrap());
            let (cx, cy) = pts[0];
            let angle = |(x, y): (f64, f64)| {
                let a = (x - cx).atan2(-(y - cy)).to_degrees();
                if a < 0.0 { a + 360.0 } else { a }
            };
            // Points are: centre, arc start, the arc radii (read as a point), arc end.
            (angle(pts[1]), angle(pts[3]))
        })
        .collect()
}

#[test]
fn slice_angles_follow_the_shares_clockwise_from_twelve_o_clock_and_sum_to_a_circle() {
    let r = ok(&shares());
    let a = slice_angles(&parse(&r.svg));
    assert_eq!(a.len(), 4);
    let want = [(0.0, 180.0), (180.0, 270.0), (270.0, 324.0), (324.0, 360.0)];
    for ((s, e), (ws, we)) in a.iter().zip(want) {
        assert!((s - ws).abs() < 0.05, "start {s} vs {ws}");
        let e = if *e == 0.0 { 360.0 } else { *e };
        assert!((e - we).abs() < 0.05, "end {e} vs {we}");
    }
    let share_sum: f64 = r
        .series
        .iter()
        .map(|s| s.extra["share"].as_f64().unwrap())
        .sum();
    assert!((share_sum - 1.0).abs() < 1e-5);
}

#[test]
fn every_slice_has_a_tooltip_with_its_value_and_share_and_the_legend_repeats_the_percent() {
    let r = ok(&shares());
    let doc = parse(&r.svg);
    let tips: Vec<String> = all(&doc, "path").into_iter().filter_map(title).collect();
    assert_eq!(
        tips,
        ["a: 50 (50%)", "b: 25 (25%)", "c: 15 (15%)", "d: 10 (10%)"]
    );
    let t = texts(&doc);
    assert!(
        t.contains(&"a (50%)".to_owned()) && t.contains(&"d (10%)".to_owned()),
        "{t:?}"
    );
}

#[test]
fn slices_below_five_percent_have_no_label_inside_and_big_ones_do() {
    let rows = [json!(["big", 96]), json!(["tiny", 4])];
    let r = ok(&with_rows("pie", "", &["k", "v"], &rows));
    let t = texts(&parse(&r.svg));
    assert!(
        t.contains(&"96%".to_owned())
            && !t.contains(&"4.0%".to_owned())
            && !t.contains(&"4%".to_owned()),
        "{t:?}"
    );
}

#[test]
fn a_single_slice_is_a_full_circle() {
    let r = ok(&with_rows("pie", "", &["k", "v"], &[json!(["only", 5])]));
    let doc = parse(&r.svg);
    let c: Vec<_> = titled(&doc, "circle", "only");
    assert_eq!(c.len(), 1);
    assert!((num(c[0], "r") - r.plot.w.min(r.plot.h) / 2.0 + 2.0).abs() < 0.5);
    let d = ok(&with_rows("donut", "", &["k", "v"], &[json!(["only", 5])]));
    let doc = parse(&d.svg);
    let ring = titled(&doc, "circle", "only")[0];
    assert!(num(ring, "stroke-width") > 10.0 && ring.attribute("fill") == Some("none"));
}

#[test]
fn a_donut_has_a_hole_and_prints_the_total_in_it() {
    let r = ok(&shares().replace(r#""chart":"pie""#, r#""chart":"donut""#));
    let doc = parse(&r.svg);
    let first = all(&doc, "path")
        .into_iter()
        .find(|p| title(*p).is_some())
        .unwrap();
    assert!(
        first.attribute("d").unwrap().matches('A').count() == 2,
        "an outer and an inner arc"
    );
    let t = texts(&doc);
    assert!(
        t.contains(&"100".to_owned()) && t.contains(&"total".to_owned()),
        "{t:?}"
    );
    assert!(r.alt.starts_with("Donut chart"));
}

#[test]
fn more_than_twelve_slices_group_the_smallest_as_other() {
    let rows: Vec<_> = (1..=15).map(|i| json!([format!("s{i}"), i])).collect();
    let r = ok(&with_rows("pie", "", &["k", "v"], &rows));
    // The eleven largest (15 down to 5) stay; 1 + 2 + 3 + 4 become one slice.
    assert_eq!(r.series.len(), 12);
    let other = r.series.last().unwrap();
    assert_eq!(other.name, "Other");
    assert_eq!(other.extra["value"], json!(10.0));
    assert_eq!(
        r.series[0].name,
        "s1".replace('1', "5"),
        "data order is kept for the slices that stay"
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("15 slices; the 4 smallest are grouped as \"Other\"")),
        "{:?}",
        r.warnings
    );
    let total: f64 = r
        .series
        .iter()
        .map(|s| s.extra["value"].as_f64().unwrap())
        .sum();
    assert_eq!(total, 120.0, "nothing is lost by grouping");
}

#[test]
fn negative_and_zero_values_are_not_drawn_and_the_warnings_say_so() {
    let rows = [
        json!(["a", 5]),
        json!(["b", -2]),
        json!(["c", 0]),
        json!(["d", null]),
        json!(["e", 5]),
    ];
    let r = ok(&with_rows("pie", "", &["k", "v"], &rows));
    assert_eq!(r.series.len(), 2);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("1 slices have a negative value")),
        "{:?}",
        r.warnings
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("2 slices have no positive value")),
        "{:?}",
        r.warnings
    );
    assert!(
        err(&with_rows(
            "pie",
            "",
            &["k", "v"],
            &[json!(["a", -1]), json!(["b", 0])]
        ))
        .contains("at least one positive value")
    );
}

#[test]
fn duplicate_labels_are_combined_and_sorting_orders_the_slices() {
    let rows = [json!(["x", 1]), json!(["y", 9]), json!(["x", 4])];
    let r = ok(&with_rows("pie", "", &["k", "v"], &rows));
    assert_eq!(
        r.series
            .iter()
            .map(|s| (s.name.as_str(), s.extra["value"].clone()))
            .collect::<Vec<_>>(),
        [("x", json!(5.0)), ("y", json!(9.0))]
    );
    let sorted = ok(&with_rows(
        "pie",
        r#""sort":"value_desc""#,
        &["k", "v"],
        &rows,
    ));
    assert_eq!(sorted.series[0].name, "y");
}

#[test]
fn the_value_column_defaults_to_the_first_numeric_one_and_extra_columns_are_noted() {
    let rows = [json!(["a", "note", 3]), json!(["b", "note", 1])];
    let r = ok(&with_rows("pie", "", &["k", "text", "n"], &rows));
    assert_eq!(r.series[0].extra["value"], json!(3.0));
    let r = ok(&with_rows(
        "pie",
        r#""y":["n","m"]"#,
        &["k", "n", "m"],
        &[json!(["a", 1, 2]), json!(["b", 3, 4])],
    ));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("uses the first column in y"))
    );
}

#[test]
fn slice_colours_follow_the_palette_and_labels_stay_readable_on_them() {
    let r = ok(&shares());
    let doc = parse(&r.svg);
    let fills: Vec<&str> = all(&doc, "path")
        .into_iter()
        .filter(|p| title(*p).is_some())
        .filter_map(|p| p.attribute("fill"))
        .collect();
    assert_eq!(fills, ["#0072b2", "#e69f00", "#009e73", "#d55e00"]);
    let ink: Vec<&str> = all(&doc, "text")
        .into_iter()
        .filter(|t| {
            t.attribute("font-weight") == Some("bold") && t.text().is_some_and(|x| x.ends_with('%'))
        })
        .filter_map(|t| t.attribute("fill"))
        .collect();
    assert_eq!(ink[0], "#ffffff", "white on the dark blue");
    assert_eq!(ink[1], "#111111", "dark on the orange");
}

#[test]
fn the_pie_is_centred_in_what_the_legend_leaves() {
    let r = ok(&shares());
    let doc = parse(&r.svg);
    let first = all(&doc, "path")
        .into_iter()
        .find(|p| title(*p).is_some())
        .unwrap();
    let (cx, cy) = path_points(first.attribute("d").unwrap())[0];
    assert!(
        (cx - (r.plot.x + r.plot.w / 2.0)).abs() < 0.02
            && (cy - (r.plot.y + r.plot.h / 2.0)).abs() < 0.02
    );
}

fn grid() -> String {
    with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &[
            json!(["r1", "c1", 10]),
            json!(["r1", "c2", 20]),
            json!(["r2", "c1", 30]),
            json!(["r2", "c2", 40]),
        ],
    )
}

fn cell_fills(doc: &roxmltree::Document<'_>) -> Vec<(String, String)> {
    all(doc, "rect")
        .into_iter()
        .filter_map(|r| Some((title(r)?, r.attribute("fill")?.to_owned())))
        .collect()
}

#[test]
fn the_lowest_and_highest_cells_take_the_ends_of_the_sequential_ramp() {
    let r = ok(&grid());
    let cells = cell_fills(&parse(&r.svg));
    assert_eq!(cells.len(), 4);
    let get = |t: &str| cells.iter().find(|c| c.0 == t).unwrap().1.clone();
    assert_eq!(get("r1, c1: 10"), hex(sequential(0.0)));
    assert_eq!(get("r2, c2: 40"), hex(sequential(1.0)));
    assert_eq!(get("r1, c2: 20"), hex(sequential(1.0 / 3.0)));
    assert_eq!(get("r2, c1: 30"), hex(sequential(2.0 / 3.0)));
}

#[test]
fn cells_tile_the_plot_in_a_grid_with_the_first_row_on_top() {
    let r = ok(&grid());
    let doc = parse(&r.svg);
    let cells: Vec<_> = all(&doc, "rect")
        .into_iter()
        .filter(|r| title(*r).is_some())
        .map(rect_of)
        .collect();
    let (cw, ch) = (r.plot.w / 2.0, r.plot.h / 2.0);
    assert!(
        (cells[0].0 - (r.plot.x + 0.5)).abs() < 0.02
            && (cells[0].1 - (r.plot.y + 0.5)).abs() < 0.02,
        "r1, c1 is top left"
    );
    assert!(
        (cells[3].0 - (r.plot.x + cw + 0.5)).abs() < 0.02
            && (cells[3].1 - (r.plot.y + ch + 0.5)).abs() < 0.02,
        "r2, c2 is bottom right"
    );
    assert!((cells[0].2 - (cw - 1.0)).abs() < 0.02 && (cells[0].3 - (ch - 1.0)).abs() < 0.02);
}

#[test]
fn small_grids_print_the_value_in_each_cell_in_a_readable_colour() {
    let r = ok(&grid());
    let doc = parse(&r.svg);
    let labels: Vec<(String, String)> = all(&doc, "text")
        .into_iter()
        .filter(|t| {
            ["10", "20", "30", "40"].contains(&t.text().unwrap_or(""))
                && t.attribute("text-anchor") == Some("middle")
        })
        .map(|t| {
            (
                t.text().unwrap().to_owned(),
                t.attribute("fill").unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(labels.len(), 4);
    assert_eq!(labels[0].1, "#ffffff", "white on dark purple");
    assert_eq!(labels[3].1, "#111111", "dark on yellow");
}

#[test]
fn a_big_grid_has_no_cell_labels() {
    let rows: Vec<_> = (0..25)
        .flat_map(|a| (0..25).map(move |b| json!([format!("r{a}"), format!("c{b}"), a * b])))
        .collect();
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &rows,
    ));
    let doc = parse(&r.svg);
    assert!(
        !texts(&doc).contains(&"48".to_owned())
            || !all(&doc, "text").iter().any(|t| t.text() == Some("48")
                && t.attribute("text-anchor") == Some("middle")
                && num(*t, "x") > r.plot.x
                && num(*t, "x") < r.plot.right())
    );
}

#[test]
fn a_missing_cell_is_drawn_empty_and_counted_not_coloured_as_zero() {
    let rows = [
        json!(["a", "x", 1]),
        json!(["a", "y", 5]),
        json!(["b", "x", 3]),
    ];
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &rows,
    ));
    let cells = cell_fills(&parse(&r.svg));
    // An empty cell is an outline, never a fill that a colour scale could also produce.
    assert!(
        cells
            .iter()
            .any(|c| c.0 == "b, y: no value" && c.1 == "none")
    );
    let doc = parse(&r.svg);
    let rect = titled(&doc, "rect", "b, y: no value");
    assert_eq!(rect[0].attribute("stroke-dasharray"), Some("3 2"));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("1 cell has no value and is drawn empty")),
        "{:?}",
        r.warnings
    );
    assert_eq!((r.series[0].points, r.series[0].gaps), (3, 1));
}

#[test]
fn data_on_both_sides_of_zero_gets_a_diverging_scale_centred_on_zero() {
    let rows = [
        json!(["a", "x", -4]),
        json!(["a", "y", 0]),
        json!(["b", "x", 2]),
        json!(["b", "y", 4]),
    ];
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &rows,
    ));
    let cells = cell_fills(&parse(&r.svg));
    let get = |t: &str| cells.iter().find(|c| c.0 == t).unwrap().1.clone();
    assert_eq!(
        get("a, y: 0"),
        hex(diverging(0.5, &LIGHT)),
        "zero is the neutral colour"
    );
    assert_eq!(get("b, y: 4"), hex(diverging(1.0, &LIGHT)));
    assert_eq!(get("a, x: -4"), hex(diverging(0.0, &LIGHT)));
    assert!(r.alt.contains("with 0 as the neutral colour"), "{}", r.alt);
}

#[test]
fn a_chosen_centre_and_the_dark_theme_change_the_colours() {
    let rows = [
        json!(["a", "x", 10]),
        json!(["a", "y", 20]),
        json!(["b", "x", 30]),
        json!(["b", "y", 40]),
    ];
    let json = with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v","center":20,"theme":"dark""#,
        &["r", "c", "v"],
        &rows,
    );
    let r = ok(&json);
    let cells = cell_fills(&parse(&r.svg));
    let get = |t: &str| cells.iter().find(|c| c.0 == t).unwrap().1.clone();
    assert_eq!(get("a, y: 20"), hex(diverging(0.5, &DARK)));
    assert_eq!(get("b, y: 40"), hex(diverging(1.0, &DARK)));
}

#[test]
fn duplicate_pairs_use_the_mean_unless_another_aggregate_is_asked_for() {
    let rows = [
        json!(["a", "x", 2]),
        json!(["a", "x", 6]),
        json!(["b", "y", 1]),
    ];
    let mean = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &rows,
    ));
    assert!(
        cell_fills(&parse(&mean.svg))
            .iter()
            .any(|c| c.0 == "a, x: 4")
    );
    let sum = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v","agg":"sum""#,
        &["r", "c", "v"],
        &rows,
    ));
    assert!(
        cell_fills(&parse(&sum.svg))
            .iter()
            .any(|c| c.0 == "a, x: 8")
    );
}

#[test]
fn a_colour_bar_shows_the_scale_with_labelled_ticks() {
    let r = ok(&grid());
    let doc = parse(&r.svg);
    let grad = all(&doc, "linearGradient");
    assert_eq!(grad.len(), 1);
    assert_eq!(all(&doc, "stop").len(), 11);
    let bar = all(&doc, "rect")
        .into_iter()
        .find(|r| r.attribute("fill") == Some("url(#scale)"))
        .unwrap();
    assert!(num(bar, "x") > r.plot.right());
    let t = texts(&doc);
    assert!(t.contains(&"10".to_owned()) && t.contains(&"40".to_owned()));
}

#[test]
fn the_axes_list_the_categories_and_sorting_applies_to_both() {
    let rows = [
        json!(["b", "y", 1]),
        json!(["a", "x", 2]),
        json!(["b", "x", 3]),
        json!(["a", "y", 4]),
    ];
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v","sort":"x""#,
        &["r", "c", "v"],
        &rows,
    ));
    let doc = parse(&r.svg);
    let row_labels: Vec<String> = all(&doc, "text")
        .into_iter()
        .filter(|t| t.attribute("text-anchor") == Some("end") && num(*t, "x") < r.plot.x)
        .map(|t| t.text().unwrap().to_owned())
        .collect();
    assert_eq!(row_labels, ["a", "b"]);
    let col_labels: Vec<String> = all(&doc, "text")
        .into_iter()
        .filter(|t| {
            t.attribute("text-anchor") == Some("middle")
                && num(*t, "y") > r.plot.bottom()
                && ["x", "y"].contains(&t.text().unwrap_or(""))
        })
        .map(|t| t.text().unwrap().to_owned())
        .collect();
    assert_eq!(col_labels, ["x", "y"]);
}

#[test]
fn more_than_eighty_categories_on_an_axis_are_capped_with_a_warning() {
    let rows: Vec<_> = (0..90)
        .map(|i| json!([format!("r{i:02}"), "c", i]))
        .collect();
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &rows,
    ));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("90 y categories; only the first 80")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_heatmap_needs_three_distinct_columns_with_numbers() {
    assert!(
        err(&with_rows("heatmap", "", &["a", "b"], &[json!(["x", "y"])]))
            .contains("needs three columns")
    );
    assert!(
        err(&with_rows(
            "heatmap",
            r#""x":"a","y":["a"],"value":"b""#,
            &["a", "b"],
            &[json!([1, 2])]
        ))
        .contains("same column")
    );
    assert!(
        err(&with_rows(
            "heatmap",
            "",
            &["a", "b", "c"],
            &[json!(["x", "y", "z"])]
        ))
        .contains("no column of numbers for the cell values")
    );
    assert!(
        err(&with_rows(
            "heatmap",
            r#""x":"a","y":["b"],"value":"c""#,
            &["a", "b", "c"],
            &[json!(["x", "y", null])]
        ))
        .contains("no numbers to colour")
    );
}

fn gappy_diverging() -> String {
    // Tue/am is exactly zero (the neutral colour) and Wed/am is missing.
    with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v""#,
        &["r", "c", "v"],
        &[
            json!(["Tue", "am", 0]),
            json!(["Tue", "pm", -4]),
            json!(["Wed", "pm", 6]),
        ],
    )
}

#[test]
fn a_missing_cell_never_looks_like_the_neutral_colour_of_a_diverging_scale() {
    let r = ok(&gappy_diverging());
    let cells = cell_fills(&parse(&r.svg));
    let zero = cells.iter().find(|c| c.0 == "Tue, am: 0").unwrap();
    let gap = cells.iter().find(|c| c.0 == "Wed, am: no value").unwrap();
    assert_eq!(zero.1, hex(diverging(0.5, &LIGHT)));
    assert_eq!(gap.1, "none");
    assert_ne!(zero.1, gap.1);
}

#[test]
fn a_heatmap_with_gaps_has_a_no_value_key_and_one_without_has_none() {
    let gappy = parse(&ok(&gappy_diverging()).svg)
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "text" && n.text() == Some("no value"))
        .count();
    assert_eq!(gappy, 1);
    let full = ok(&grid());
    let doc = parse(&full.svg);
    assert!(!texts(&doc).iter().any(|t| t == "no value"));
}

#[test]
fn heatmap_legend_placement_is_reported_when_it_cannot_be_honoured() {
    for place in ["left", "top", "bottom"] {
        let json = gappy_diverging().replacen("{", &format!("{{\"legend\":\"{place}\","), 1);
        let r = ok(&json);
        assert!(
            r.warnings.iter().any(|w| w
                == "legend placement is ignored for a heatmap; the colour bar stays on the right"),
            "{place}: {:?}",
            r.warnings
        );
    }
    for place in ["right", "auto"] {
        let json = gappy_diverging().replacen("{", &format!("{{\"legend\":\"{place}\","), 1);
        assert!(
            ok(&json)
                .warnings
                .iter()
                .all(|w| !w.contains("legend placement")),
            "{place}"
        );
    }
}

#[test]
fn legend_none_removes_the_colour_bar_and_widens_the_plot() {
    let with = ok(&gappy_diverging());
    let json = gappy_diverging().replacen("{", "{\"legend\":\"none\",", 1);
    let without = ok(&json);
    assert!(with.svg.contains("url(#scale)") && !without.svg.contains("url(#scale)"));
    assert!(
        without.plot.w > with.plot.w + 60.0,
        "{} vs {}",
        without.plot.w,
        with.plot.w
    );
}
