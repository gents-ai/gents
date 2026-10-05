//! The PNG: size, background, colours at known coordinates and a hash of the
//! decoded pixels of one picture per chart type.

use std::fmt::Write as _;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::palette::{self, DARK, LIGHT};
use crate::raster;
use crate::testkit::*;

struct Img {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

impl Img {
    fn px(&self, x: f64, y: f64) -> (u8, u8, u8) {
        let (xi, yi) = (x.floor() as usize, y.floor() as usize);
        let i = (yi * self.w + xi) * 3;
        (self.rgb[i], self.rgb[i + 1], self.rgb[i + 2])
    }
}

fn png_of(r: &crate::output::Rendered) -> Img {
    let p = r.png.as_ref().expect("a PNG");
    let (w, h, rgb) = raster::decode(&p.bytes).unwrap();
    Img {
        w: w as usize,
        h: h as usize,
        rgb,
    }
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tol: i32) -> bool {
    [(a.0, b.0), (a.1, b.1), (a.2, b.2)]
        .iter()
        .all(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() <= tol)
}

fn bar_chart(extra: &str) -> String {
    with_rows(
        "bar",
        extra,
        &["k", "v"],
        &[json!(["a", 10]), json!(["b", 30]), json!(["c", 20])],
    )
}

#[test]
fn the_png_size_is_the_logical_size_times_the_scale() {
    for (w, h, s) in [
        (300, 200, 1.0),
        (300, 200, 2.0),
        (333, 211, 1.5),
        (640, 360, 0.5),
        (200, 150, 4.0),
    ] {
        let r = ok(&bar_chart(&format!(
            r#""width":{w},"height":{h},"scale":{s}"#
        )));
        let img = png_of(&r);
        assert_eq!(
            (img.w, img.h),
            (
                (f64::from(w) * s).round() as usize,
                (f64::from(h) * s).round() as usize
            ),
            "{w}x{h} at {s}"
        );
        assert_eq!(img.rgb.len(), img.w * img.h * 3);
    }
}

#[test]
fn the_png_is_the_rendering_of_the_returned_svg() {
    for chart in ["line", "bar", "pie", "scatter"] {
        let r = ok(&with_rows(
            chart,
            r#""title":"T","scale":1.5"#,
            &["x", "y"],
            &[json!([1, 5]), json!([2, 9]), json!([3, 4])],
        ));
        assert_eq!(
            raster::render(&r.svg, 1.5).unwrap().bytes,
            r.png.as_ref().unwrap().bytes,
            "{chart}"
        );
    }
}

#[test]
fn the_corner_is_the_theme_background_and_the_picture_is_not_blank() {
    for (theme, bg) in [("light", LIGHT.bg), ("dark", DARK.bg)] {
        for chart in [
            "line",
            "area",
            "bar",
            "scatter",
            "histogram",
            "box",
            "pie",
            "donut",
            "heatmap",
        ] {
            let shape = match chart {
                "scatter" => r#""x":"n","y":["y"]"#,
                "histogram" => r#""x":"y""#,
                "heatmap" => r#""x":"x","y":["z"],"value":"y""#,
                _ => r#""x":"x","y":["y"]"#,
            };
            let extra = format!(r#""theme":"{theme}","width":320,"height":240,{shape}"#);
            let rows = [
                json!(["a", "p", 5, 1]),
                json!(["b", "q", 9, 2]),
                json!(["c", "p", 4, 3]),
                json!(["d", "q", 12, 4]),
            ];
            let r = ok(&with_rows(chart, &extra, &["x", "z", "y", "n"], &rows));
            let img = png_of(&r);
            let bg = palette::parse_hex(bg).unwrap();
            assert_eq!(img.px(0.0, 0.0), bg, "{chart} {theme}: the corner");
            assert_eq!(img.px(img.w as f64 - 1.0, img.h as f64 - 1.0), bg);
            let inked = img
                .rgb
                .as_chunks::<3>()
                .0
                .iter()
                .filter(|p| (p[0], p[1], p[2]) != bg)
                .count();
            assert!(
                inked as f64 > 0.02 * (img.w * img.h) as f64,
                "{chart} {theme}: only {inked} pixels differ from the background"
            );
            let distinct: std::collections::BTreeSet<[u8; 3]> =
                img.rgb.as_chunks::<3>().0.iter().copied().collect();
            assert!(
                distinct.len() >= 6,
                "{chart} {theme}: {} colours",
                distinct.len()
            );
        }
    }
}

#[test]
fn the_middle_of_each_bar_has_exactly_its_series_colour_at_every_scale() {
    for scale in [1.0, 2.0] {
        let r = ok(&bar_chart(&format!(
            r#""scale":{scale},"width":400,"height":300"#
        )));
        let img = png_of(&r);
        let doc = parse(&r.svg);
        for b in titled(&doc, "rect", "v, ") {
            let (x, y, w, h) = rect_of(b);
            assert_eq!(
                img.px((x + w / 2.0) * scale, (y + h / 2.0) * scale),
                palette::parse_hex("#0072b2").unwrap(),
                "scale {scale}"
            );
        }
    }
}

#[test]
fn grouped_bars_keep_their_own_colours_in_the_picture() {
    let r = ok(&with_rows(
        "bar",
        r#""width":400,"height":300"#,
        &["k", "p", "q"],
        &[json!(["a", 10, 20]), json!(["b", 30, 5])],
    ));
    let img = png_of(&r);
    let doc = parse(&r.svg);
    for (series, color) in [("p", "#0072b2"), ("q", "#e69f00")] {
        for b in titled(&doc, "rect", &format!("{series}, ")) {
            let (x, y, w, h) = rect_of(b);
            assert_eq!(
                img.px(x + w / 2.0, y + h - 2.0),
                palette::parse_hex(color).unwrap(),
                "{series}"
            );
        }
    }
}

#[test]
fn the_centre_of_a_pie_slice_has_the_slice_colour() {
    let r = ok(&with_rows(
        "pie",
        r#""width":500,"height":400"#,
        &["k", "v"],
        &[json!(["a", 50]), json!(["b", 30]), json!(["c", 20])],
    ));
    let img = png_of(&r);
    let doc = parse(&r.svg);
    let first = all(&doc, "path")
        .into_iter()
        .find(|p| title(*p).is_some())
        .unwrap();
    let (cx, cy) = path_points(first.attribute("d").unwrap())[0];
    let radius = r.plot.w.min(r.plot.h) / 2.0 - 2.0;
    // Slice a spans 0 to 180 degrees: its middle is at three o'clock; slice b's is 234 degrees.
    assert_eq!(
        img.px(cx + radius * 0.9, cy),
        palette::parse_hex("#0072b2").unwrap()
    );
    let (s, c) = crate::det::sin_cos(crate::det::radians(234.0));
    assert_eq!(
        img.px(cx + radius * 0.9 * s, cy - radius * 0.9 * c),
        palette::parse_hex("#e69f00").unwrap()
    );
}

#[test]
fn heatmap_cells_are_filled_with_their_ramp_colour() {
    let rows = [
        json!(["a", "x", 1]),
        json!(["a", "y", 5]),
        json!(["b", "x", 9]),
        json!(["b", "y", 3]),
    ];
    let r = ok(&with_rows(
        "heatmap",
        r#""x":"c","y":["r"],"value":"v","width":400,"height":300"#,
        &["r", "c", "v"],
        &rows,
    ));
    let img = png_of(&r);
    let doc = parse(&r.svg);
    for cell in all(&doc, "rect")
        .into_iter()
        .filter(|c| title(*c).is_some())
    {
        let (x, y, _, _) = rect_of(cell);
        let fill = palette::parse_hex(cell.attribute("fill").unwrap()).unwrap();
        assert_eq!(img.px(x + 4.0, y + 4.0), fill, "{}", title(cell).unwrap());
    }
}

#[test]
fn a_scatter_dot_is_the_series_colour_blended_with_the_background() {
    let r = ok(&with_rows(
        "scatter",
        r#""width":400,"height":300,"x":"x","y":["y"]"#,
        &["x", "y"],
        &[json!([1, 1]), json!([5, 8])],
    ));
    let img = png_of(&r);
    let doc = parse(&r.svg);
    let c = all(&doc, "circle")
        .into_iter()
        .find(|c| c.attribute("r") == Some("4"))
        .unwrap();
    let blend = |fg: u8, bg: u8| (0.78 * f64::from(fg) + 0.22 * f64::from(bg)).round() as u8;
    let (fg, bg) = (
        palette::parse_hex("#0072b2").unwrap(),
        palette::parse_hex(LIGHT.bg).unwrap(),
    );
    let want = (blend(fg.0, bg.0), blend(fg.1, bg.1), blend(fg.2, bg.2));
    assert!(
        near(img.px(num(c, "cx"), num(c, "cy")), want, 2),
        "{:?} vs {want:?}",
        img.px(num(c, "cx"), num(c, "cy"))
    );
}

#[test]
fn a_line_is_drawn_in_its_colour_between_its_points() {
    let r = ok(&with_rows(
        "line",
        r#""width":400,"height":300,"x":"x","y":["y"]"#,
        &["x", "y"],
        &[json!([0, 0]), json!([10, 10])],
    ));
    let img = png_of(&r);
    let doc = parse(&r.svg);
    let p = &stroked_paths(&doc, "#0072b2")[0];
    let mid = ((p[0].0 + p[1].0) / 2.0, (p[0].1 + p[1].1) / 2.0);
    assert!(
        near(
            img.px(mid.0, mid.1),
            palette::parse_hex("#0072b2").unwrap(),
            12
        ),
        "{:?}",
        img.px(mid.0, mid.1)
    );
    assert_eq!(
        img.px(mid.0 + 60.0, mid.1 - 60.0),
        palette::parse_hex(LIGHT.bg).unwrap(),
        "away from the line it is the background"
    );
}

#[test]
fn text_is_drawn_in_the_text_colour_and_the_dark_theme_flips_it() {
    for (theme, fg) in [("light", LIGHT.fg), ("dark", DARK.fg)] {
        let r = ok(&bar_chart(&format!(
            r#""title":"Heading","theme":"{theme}","width":400,"height":300"#
        )));
        let img = png_of(&r);
        let fg = palette::parse_hex(fg).unwrap();
        let in_title = (16..38)
            .flat_map(|y| (14..100).map(move |x| (x, y)))
            .filter(|(x, y)| near(img.px(f64::from(*x), f64::from(*y)), fg, 8))
            .count();
        assert!(
            in_title > 60,
            "{theme}: {in_title} title pixels in the text colour"
        );
    }
}

#[test]
fn a_larger_scale_adds_detail_not_a_different_picture() {
    let one = png_of(&ok(&bar_chart(r#""width":300,"height":200,"scale":1"#)));
    let two = png_of(&ok(&bar_chart(r#""width":300,"height":200,"scale":2"#)));
    // The background and the bars are flat colour: those samples agree between the two.
    let mut agree = 0;
    let mut flat = 0;
    for y in (4..196).step_by(8) {
        for x in (4..296).step_by(8) {
            let a = one.px(f64::from(x), f64::from(y));
            let n = (1..=3)
                .filter(|d| one.px(f64::from(x + d), f64::from(y)) == a)
                .count();
            if n == 3 {
                flat += 1;
                agree += usize::from(two.px(f64::from(x) * 2.0, f64::from(y) * 2.0) == a);
            }
        }
    }
    assert!(flat > 100 && agree * 100 >= flat * 97, "{agree} of {flat}");
}

fn canonical() -> Vec<(&'static str, String)> {
    let months = ["Jan", "Feb", "Mar", "Apr", "May"];
    let wide: Vec<serde_json::Value> = months
        .iter()
        .enumerate()
        .map(|(i, m)| json!([m, 10 + i * 3, 25 - i * 2, (i * 7) % 11]))
        .collect();
    let cols = ["m", "a", "b", "c"];
    let mk = |chart: &str, extra: &str| {
        with_rows(
            chart,
            &format!(r#""title":"{chart}","width":480,"height":320,{extra}"#),
            &cols,
            &wide,
        )
    };
    vec![
        ("line", mk("line", r#""y":["a","b"]"#)),
        ("area", mk("area", r#""y":["a"]"#)),
        ("stacked_area", mk("stacked_area", r#""y":["a","b","c"]"#)),
        ("bar", mk("bar", r#""y":["a","b"]"#)),
        ("stacked_bar", mk("stacked_bar", r#""y":["a","b"]"#)),
        (
            "horizontal_bar",
            mk("horizontal_bar", r#""y":["a"],"sort":"value_desc""#),
        ),
        ("combo", mk("combo", r#""y":["a"],"line":["b"]"#)),
        ("scatter", mk("scatter", r#""x":"a","y":["b"]"#)),
        ("bubble", mk("bubble", r#""x":"a","y":["b"],"size":"c""#)),
        ("histogram", mk("histogram", r#""x":"a","bins":4"#)),
        ("box", mk("box", r#""y":["a","b","c"],"theme":"dark""#)),
        ("pie", mk("pie", r#""y":["a"]"#)),
        ("donut", mk("donut", r#""y":["a"],"theme":"dark""#)),
        ("heatmap", mk("heatmap", r#""x":"m","y":["c"],"value":"a""#)),
    ]
}

fn pixel_hash(r: &crate::output::Rendered) -> String {
    let img = png_of(r);
    let mut h = Sha256::new();
    h.update((img.w as u32).to_be_bytes());
    h.update((img.h as u32).to_be_bytes());
    h.update(&img.rgb);
    h.finalize().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[test]
fn the_decoded_pixels_of_one_chart_per_type_match_their_recorded_hashes() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/pixels.txt");
    let dump = std::env::var_os("DUMP_PNG_DIR");
    let got: String = canonical()
        .iter()
        .map(|(name, req)| {
            let r = ok(req);
            if let Some(dir) = &dump {
                let png = r.png.as_ref().map(|p| p.bytes.clone()).unwrap_or_default();
                std::fs::write(std::path::Path::new(dir).join(format!("{name}.png")), png).unwrap();
            }
            format!("{name} {}\n", pixel_hash(&r))
        })
        .collect();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(path, &got).unwrap();
    }
    let want = std::fs::read_to_string(path).expect("tests/golden/pixels.txt exists; record it with UPDATE_GOLDEN=1 after reviewing the pictures");
    assert_eq!(
        got, want,
        "a picture changed; review it and re-record with UPDATE_GOLDEN=1"
    );
}
