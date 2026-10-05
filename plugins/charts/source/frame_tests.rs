//! Layout: margins, titles, legends and label placement.

use serde_json::json;

use crate::axes;
use crate::ctx::Ctx;
use crate::frame::*;
use crate::spec::{Legend, Request, Spec};
use crate::testkit::*;

fn spec(json: &str) -> Spec {
    serde_json::from_str::<Request>(json)
        .unwrap()
        .resolve()
        .unwrap()
}

fn num_axis(lo: f64, hi: f64, target: usize) -> Axis {
    axes::numeric(
        axes::NumSpec {
            min: lo,
            max: hi,
            user_min: None,
            user_max: None,
            zero: false,
            nice: true,
            log: false,
        },
        target,
        None,
        None,
    )
}

fn items(n: usize, label: &str) -> Vec<LegendItem> {
    (0..n)
        .map(|i| LegendItem {
            label: format!("{label}{i}"),
            color: "#000000".into(),
            swatch: Swatch::Box,
        })
        .collect()
}

fn lay(
    s: &Spec,
    labels: Vec<String>,
    legend: &[LegendItem],
    auto: Legend,
) -> Result<(Laid, Vec<String>), String> {
    let mut ctx = Ctx::new(s);
    let xf = |_| axes::band(labels.clone(), Some("categories".into()));
    let yf = |t| num_axis(0.0, 100.0, t);
    let laid = layout(
        &mut ctx,
        &FrameSpec {
            x: &xf,
            y: &yf,
            y2: None,
            legend,
            extra_right: 0.0,
            auto_legend: auto,
        },
    )
    .map_err(|e| e.0)?;
    Ok((laid, ctx.notes.into_vec()))
}

#[test]
fn the_plot_stays_inside_the_canvas_with_the_outer_margin() {
    for (w, h) in [
        (200, 150),
        (400, 300),
        (800, 480),
        (1600, 900),
        (4000, 4000),
    ] {
        let titles = if w >= 400 {
            r#","title":"T","subtitle":"S""#
        } else {
            ""
        };
        let s = spec(&format!(
            r#"{{"chart":"bar","width":{w},"height":{h}{titles}}}"#
        ));
        let (laid, _) = lay(&s, vec!["a".into(), "b".into()], &[], Legend::None).unwrap();
        let p = laid.frame.plot;
        assert!(
            p.x >= PAD
                && p.y >= PAD
                && p.right() <= f64::from(w) - PAD + 0.001
                && p.bottom() <= f64::from(h) - PAD + 0.001,
            "{w}x{h}: {p:?}"
        );
        assert!(p.w > 60.0 && p.h > 60.0);
    }
}

#[test]
fn a_title_and_subtitle_push_the_plot_down() {
    let none = lay(
        &spec(r#"{"chart":"bar"}"#),
        vec!["a".into()],
        &[],
        Legend::None,
    )
    .unwrap()
    .0
    .frame
    .plot;
    let title = lay(
        &spec(r#"{"chart":"bar","title":"T"}"#),
        vec!["a".into()],
        &[],
        Legend::None,
    )
    .unwrap()
    .0
    .frame
    .plot;
    let both = lay(
        &spec(r#"{"chart":"bar","title":"T","subtitle":"S"}"#),
        vec!["a".into()],
        &[],
        Legend::None,
    )
    .unwrap()
    .0
    .frame
    .plot;
    assert!(none.y < title.y && title.y < both.y);
    assert_eq!(none.right(), title.right());
}

#[test]
fn a_title_that_is_too_wide_shrinks_first_then_is_shortened_with_a_note() {
    let long = "A very long title that goes on and on about the quarterly numbers of every region";
    let s = spec(&format!(
        r#"{{"chart":"bar","width":400,"title":{}}}"#,
        json!(long)
    ));
    let (_, notes) = lay(&s, vec!["a".into()], &[], Legend::None).unwrap();
    assert!(
        notes.iter().any(|n| n.contains("title is too long")),
        "{notes:?}"
    );
    let r = ok(&format!(
        r#"{{"chart":"bar","width":400,"title":{},"data":"k,v\na,1\n"}}"#,
        json!(long)
    ));
    let doc = parse(&r.svg);
    let g = all(&doc, "g")
        .into_iter()
        .find(|g| title(*g).is_some_and(|t| t == long))
        .expect("the full title is the tooltip of the shortened one");
    let shown = all(&doc, "text")
        .into_iter()
        .find(|t| t.parent() == Some(g))
        .unwrap();
    assert!(shown.text().unwrap().ends_with('\u{2026}'));
    assert_eq!(
        shown.attribute("font-size"),
        Some("14"),
        "it shrank to the smallest size first"
    );
    let fits = ok(r#"{"chart":"bar","title":"Short title","data":"k,v\na,1\n"}"#);
    assert_eq!(
        all(&parse(&fits.svg), "text")
            .into_iter()
            .find(|t| t.text() == Some("Short title"))
            .unwrap()
            .attribute("font-size"),
        Some("18")
    );
}

#[test]
fn legend_sides_take_room_from_the_plot() {
    let legend = items(3, "series");
    let none = lay(
        &spec(r#"{"chart":"bar","legend":"none"}"#),
        vec!["a".into()],
        &legend,
        Legend::Right,
    )
    .unwrap()
    .0
    .frame
    .plot;
    let right = lay(
        &spec(r#"{"chart":"bar","legend":"right"}"#),
        vec!["a".into()],
        &legend,
        Legend::None,
    )
    .unwrap()
    .0
    .frame
    .plot;
    let left = lay(
        &spec(r#"{"chart":"bar","legend":"left"}"#),
        vec!["a".into()],
        &legend,
        Legend::None,
    )
    .unwrap()
    .0
    .frame
    .plot;
    let top = lay(
        &spec(r#"{"chart":"bar","legend":"top"}"#),
        vec!["a".into()],
        &legend,
        Legend::None,
    )
    .unwrap()
    .0
    .frame
    .plot;
    let bottom = lay(
        &spec(r#"{"chart":"bar","legend":"bottom"}"#),
        vec!["a".into()],
        &legend,
        Legend::None,
    )
    .unwrap()
    .0
    .frame
    .plot;
    assert!(right.w < none.w && right.x == none.x);
    assert!(left.w < none.w && left.x > none.x);
    assert!(top.h < none.h && top.y > none.y);
    assert!(bottom.h < none.h && bottom.y == none.y);
}

#[test]
fn the_automatic_legend_follows_the_series_count_and_label_length() {
    let s = spec(r#"{"chart":"bar"}"#);
    let one = lay(&s, vec!["a".into()], &items(1, "s"), Legend::Auto)
        .unwrap()
        .0;
    assert!(
        one.frame.legend.is_empty(),
        "a single series needs no legend"
    );
    let few = lay(&s, vec!["a".into()], &items(4, "s"), Legend::Auto)
        .unwrap()
        .0;
    assert_eq!(few.frame.legend.len(), 4);
    assert!(
        few.frame.legend[0].x > few.frame.plot.right(),
        "on the right"
    );
    let many = lay(&s, vec!["a".into()], &items(14, "series "), Legend::Auto)
        .unwrap()
        .0;
    assert!(
        many.frame.legend[0].y > many.frame.plot.bottom(),
        "below when there are many"
    );
    let wide = lay(
        &s,
        vec!["a".into()],
        &items(3, "a legend label that is quite long indeed "),
        Legend::Auto,
    )
    .unwrap()
    .0;
    assert!(
        wide.frame.legend[0].y > wide.frame.plot.bottom(),
        "below when the labels are wide"
    );
}

#[test]
fn a_tall_legend_wraps_into_columns_instead_of_running_off_the_canvas() {
    let s = spec(r#"{"chart":"bar","height":220,"legend":"right"}"#);
    let (laid, _) = lay(&s, vec!["a".into()], &items(24, "series "), Legend::Auto).unwrap();
    let ys: Vec<f64> = laid.frame.legend.iter().map(|l| l.y).collect();
    assert!(ys.iter().all(|y| *y < 220.0 - PAD), "{ys:?}");
    let xs: std::collections::BTreeSet<i64> =
        laid.frame.legend.iter().map(|l| l.x as i64).collect();
    assert!(xs.len() > 1, "more than one column");
}

#[test]
fn a_wide_legend_at_the_bottom_wraps_into_rows_centred_in_the_width() {
    let s = spec(r#"{"chart":"bar","width":420,"legend":"bottom"}"#);
    let (laid, _) = lay(
        &s,
        vec!["a".into()],
        &items(10, "a long series name "),
        Legend::Auto,
    )
    .unwrap();
    let rows: std::collections::BTreeSet<i64> =
        laid.frame.legend.iter().map(|l| l.y as i64).collect();
    assert!(rows.len() >= 2, "{rows:?}");
    assert!(
        laid.frame.legend.iter().all(|l| l.x >= PAD - 0.001),
        "no entry starts left of the margin"
    );
}

#[test]
fn a_legend_label_that_is_too_long_is_shortened_with_its_full_text_kept() {
    let s = spec(r#"{"chart":"bar","legend":"right"}"#);
    let long = "x".repeat(120);
    let legend = vec![
        LegendItem {
            label: long.clone(),
            color: "#000000".into(),
            swatch: Swatch::Line,
        },
        LegendItem {
            label: "b".into(),
            color: "#111111".into(),
            swatch: Swatch::Dot,
        },
    ];
    let (laid, _) = lay(&s, vec!["a".into()], &legend, Legend::Auto).unwrap();
    assert!(laid.frame.legend[0].text.ends_with('\u{2026}'));
    assert_eq!(laid.frame.legend[0].full.as_deref(), Some(long.as_str()));
    assert_eq!(laid.frame.legend[1].full, None);
}

#[test]
fn a_canvas_too_small_for_its_labels_is_an_error_with_advice() {
    let s = spec(
        r#"{"chart":"bar","width":200,"height":150,"title":"T","subtitle":"S","legend":"right"}"#,
    );
    let e = lay(
        &s,
        vec!["a".into()],
        &items(5, "a long label to take the room "),
        Legend::Auto,
    )
    .err()
    .unwrap();
    assert!(
        e.contains("too small") && e.contains("make it larger"),
        "{e}"
    );
}

#[test]
fn numeric_axis_labels_never_overlap_whatever_the_width() {
    for w in (200..1400).step_by(50) {
        let s = spec(&format!(r#"{{"chart":"line","width":{w},"height":480}}"#));
        let mut ctx = Ctx::new(&s);
        let xf = |t| num_axis(0.0, 1_000_000.0, t);
        let yf = |t| num_axis(0.0, 100.0, t);
        let laid = layout(
            &mut ctx,
            &FrameSpec {
                x: &xf,
                y: &yf,
                y2: None,
                legend: &[],
                extra_right: 0.0,
                auto_legend: Legend::None,
            },
        )
        .unwrap();
        let mut end = f64::NEG_INFINITY;
        for l in &laid.frame.x.labels {
            let wd = crate::text::width(&l.text, TICK_SIZE, false);
            assert!(
                l.pos - wd / 2.0 >= end + 5.9 || end == f64::NEG_INFINITY,
                "width {w}: label {} overlaps the previous one",
                l.text
            );
            end = l.pos + wd / 2.0;
        }
        assert!(laid.frame.x.labels.len() >= 2);
    }
}

#[test]
fn a_short_canvas_gets_fewer_y_ticks_so_labels_do_not_collide() {
    let s = spec(r#"{"chart":"line","width":600,"height":160}"#);
    let mut ctx = Ctx::new(&s);
    let xf = |t| num_axis(0.0, 10.0, t);
    let yf = |t| num_axis(0.0, 1000.0, t);
    let laid = layout(
        &mut ctx,
        &FrameSpec {
            x: &xf,
            y: &yf,
            y2: None,
            legend: &[],
            extra_right: 0.0,
            auto_legend: Legend::None,
        },
    )
    .unwrap();
    let ys: Vec<f64> = laid.frame.y.labels.iter().map(|l| l.pos).collect();
    assert!(ys.windows(2).all(|w| (w[0] - w[1]).abs() >= 15.0), "{ys:?}");
}

#[test]
fn category_labels_that_fit_are_drawn_flat_without_shortening() {
    let (laid, _) = lay(
        &spec(r#"{"chart":"bar"}"#),
        vec!["North".into(), "South".into(), "East".into()],
        &[],
        Legend::None,
    )
    .unwrap();
    assert_eq!(laid.frame.x.rotate, 0.0);
    assert!(laid.frame.x.labels.iter().all(|l| l.full.is_none()));
    assert_eq!(
        laid.frame
            .x
            .labels
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>(),
        ["North", "South", "East"]
    );
}

#[test]
fn labels_a_little_too_wide_are_shortened_flat_and_much_too_wide_ones_rotate() {
    let slot_labels =
        |text: &str, n: usize| (0..n).map(|i| format!("{text}{i}")).collect::<Vec<_>>();
    let (a, _) = lay(
        &spec(r#"{"chart":"bar"}"#),
        slot_labels("Department ", 12),
        &[],
        Legend::None,
    )
    .unwrap();
    assert_eq!(a.frame.x.rotate, 0.0);
    assert!(a.frame.x.labels.iter().any(|l| l.full.is_some()));
    let (b, _) = lay(
        &spec(r#"{"chart":"bar"}"#),
        slot_labels("Operations and maintenance of regional facilities ", 8),
        &[],
        Legend::None,
    )
    .unwrap();
    assert_eq!(b.frame.x.rotate, -45.0);
}

#[test]
fn rotated_labels_are_thinned_when_the_slots_are_too_narrow_and_the_note_says_which() {
    let (laid, notes) = lay(
        &spec(r#"{"chart":"bar"}"#),
        (0..120).map(|i| format!("label number {i}")).collect(),
        &[],
        Legend::None,
    )
    .unwrap();
    assert_eq!(laid.frame.x.rotate, -45.0);
    assert!(laid.frame.x.labels.len() < 120 && laid.frame.x.labels.len() > 20);
    let gaps: Vec<f64> = laid
        .frame
        .x
        .labels
        .windows(2)
        .map(|w| w[1].pos - w[0].pos)
        .collect();
    assert!(gaps.iter().all(|g| *g >= 19.9), "{gaps:?}");
    assert!(
        notes
            .iter()
            .any(|n| n.contains("only every") && n.contains("of 120 category labels")),
        "{notes:?}"
    );
}

#[test]
fn ordinals_read_naturally() {
    for (n, s) in [
        (1, "1st"),
        (2, "2nd"),
        (3, "3rd"),
        (4, "4th"),
        (11, "11th"),
        (12, "12th"),
        (13, "13th"),
        (21, "21st"),
        (22, "22nd"),
        (23, "23rd"),
        (101, "101st"),
        (111, "111th"),
    ] {
        assert_eq!(ordinal(n), s);
    }
}

#[test]
fn axis_titles_sit_outside_the_tick_labels_and_inside_the_canvas() {
    let r = ok(
        r#"{"chart":"bar","title":"T","x_label":"Categories","y_label":"Amount (EUR)","y2_label":"","data":"k,v\nalpha,1000\nbeta,2000\n"}"#,
    );
    let doc = parse(&r.svg);
    let xt = all(&doc, "text")
        .into_iter()
        .find(|t| t.text() == Some("Categories"))
        .unwrap();
    assert!(
        num(xt, "y") > r.plot.bottom() + 20.0 && num(xt, "y") < 480.0,
        "below the tick labels, above the edge"
    );
    let yt = all(&doc, "text")
        .into_iter()
        .find(|t| t.text() == Some("Amount (EUR)"))
        .unwrap();
    assert!(
        num(yt, "x") < r.plot.x - 20.0 && num(yt, "x") >= 0.0,
        "left of the tick labels"
    );
    assert_eq!(
        yt.attribute("transform")
            .map(|t| t.starts_with("rotate(-90")),
        Some(true)
    );
}

#[test]
fn a_second_axis_reserves_room_on_the_right_for_labels_and_its_title() {
    let one = ok(r#"{"chart":"bar","data":"k,v,w\na,1,0.5\nb,2,0.7\n"}"#);
    let two = ok(
        r#"{"chart":"combo","line":["w"],"y2_label":"Rate","data":"k,v,w\na,1,0.5\nb,2,0.7\n"}"#,
    );
    assert!(two.plot.right() < one.plot.right());
    let doc = parse(&two.svg);
    let t = all(&doc, "text")
        .into_iter()
        .find(|t| t.text() == Some("Rate"))
        .unwrap();
    assert!(num(t, "x") > two.plot.right() && num(t, "x") < 800.0);
    assert_eq!(
        t.attribute("transform").map(|t| t.starts_with("rotate(90")),
        Some(true)
    );
}

#[test]
fn the_first_and_last_tick_labels_of_a_numeric_axis_stay_inside_the_canvas() {
    let r = ok(r#"{"chart":"line","width":300,"data":"x,y\n0,1\n1000000,2\n"}"#);
    let doc = parse(&r.svg);
    for t in all(&doc, "text")
        .into_iter()
        .filter(|t| t.attribute("text-anchor") == Some("middle") && num(*t, "y") > r.plot.bottom())
    {
        let w = crate::text::width(t.text().unwrap(), TICK_SIZE, false);
        assert!(
            num(t, "x") - w / 2.0 >= 0.0 && num(t, "x") + w / 2.0 <= 300.0,
            "{:?} sticks out",
            t.text()
        );
    }
}
