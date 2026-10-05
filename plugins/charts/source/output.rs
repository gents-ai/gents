//! The result of a call: the JSON the caller reads, the PNG as an image part,
//! and the output budget that keeps both under the host's cap.

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::common::SeriesInfo;
use crate::err::{Res, fail};
use crate::num::round_sig;
use crate::raster::{self, Png};
use crate::spec::Output;

/// Bytes of JSON a tool result may take: the host caps output at 4 MiB.
pub const BUDGET: usize = 3_700_000;
/// Bytes of JSON a graph record may take. The record is stored as one
/// document, so it is held well below the output cap.
pub const RECORD_BUDGET: usize = 2_000_000;

/// A file written into the bound folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Path as named in the request.
    pub path: String,
    /// Size in bytes.
    pub bytes: usize,
}

/// A rendered chart ready to be delivered.
pub struct Rendered {
    /// Chart type name.
    pub chart: &'static str,
    /// The SVG document.
    pub svg: String,
    /// The PNG, unless only the SVG was asked for.
    pub png: Option<Png>,
    /// Pixel density of the PNG.
    pub scale: f64,
    /// Logical width.
    pub width: u32,
    /// Logical height.
    pub height: u32,
    /// Description for readers without vision.
    pub alt: String,
    /// The series.
    pub series: Vec<SeriesInfo>,
    /// Warnings.
    pub warnings: Vec<String>,
    /// The rectangle the data is drawn in.
    pub plot: crate::frame::Rect,
}

fn num(v: f64) -> Value {
    if v.is_finite() && v.abs() < 1e15 {
        json!(round_sig(v, 12))
    } else {
        json!(v)
    }
}

fn series_json(s: &SeriesInfo) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(crate::text::limit_chars(&s.name, 200)));
    m.insert("mark".into(), json!(s.mark));
    m.insert("color".into(), json!(s.color));
    m.insert("axis".into(), json!(s.axis));
    m.insert("points".into(), json!(s.points));
    m.insert("gaps".into(), json!(s.gaps));
    if s.drawn != s.points && s.drawn != 0 {
        m.insert("drawn".into(), json!(s.drawn));
    }
    if let Some(v) = s.min {
        m.insert("min".into(), num(v));
    }
    if let Some(v) = s.max {
        m.insert("max".into(), num(v));
    }
    for (k, v) in &s.extra {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

fn assemble(
    r: &Rendered,
    output: Output,
    with_svg: bool,
    files: &[Written],
    warnings: &[String],
) -> Value {
    let mut resp = Map::new();
    resp.insert("chart".into(), json!(r.chart));
    resp.insert("width".into(), json!(r.width));
    resp.insert("height".into(), json!(r.height));
    resp.insert("alt".into(), json!(r.alt));
    resp.insert(
        "series".into(),
        Value::Array(r.series.iter().map(series_json).collect()),
    );
    resp.insert("warnings".into(), json!(warnings));
    if with_svg && output != Output::Png {
        resp.insert("svg".into(), json!(r.svg));
    }
    if let Some(p) = &r.png {
        resp.insert(
            "png".into(),
            json!({"width": p.width, "height": p.height, "bytes": p.bytes.len()}),
        );
    }
    if !files.is_empty() {
        resp.insert(
            "files".into(),
            Value::Array(
                files
                    .iter()
                    .map(|f| json!({"path": f.path, "bytes": f.bytes}))
                    .collect(),
            ),
        );
    }
    let mut out = Map::new();
    out.insert("response".into(), Value::Object(resp));
    if output != Output::Svg
        && let Some(p) = &r.png
    {
        let data = base64::engine::general_purpose::STANDARD.encode(&p.bytes);
        out.insert(
            "parts".into(),
            json!([{"type": "image", "data": data, "mimeType": "image/png"}]),
        );
    }
    Value::Object(out)
}

/// A graph record: the same facts as a flat object, the PNG as base64 text
/// and the series as JSON text, so it can be stored as plain fields.
fn record(
    r: &Rendered,
    output: Output,
    with_svg: bool,
    files: &[Written],
    warnings: &[String],
) -> Value {
    let mut m = Map::new();
    m.insert("chart".into(), json!(r.chart));
    m.insert("width".into(), json!(r.width));
    m.insert("height".into(), json!(r.height));
    m.insert("alt".into(), json!(r.alt));
    m.insert(
        "series_json".into(),
        json!(Value::Array(r.series.iter().map(series_json).collect()).to_string()),
    );
    m.insert("warnings".into(), json!(warnings));
    if with_svg && output != Output::Png {
        m.insert("svg".into(), json!(r.svg));
    }
    if let (Some(p), true) = (&r.png, output != Output::Svg) {
        m.insert(
            "png_base64".into(),
            json!(base64::engine::general_purpose::STANDARD.encode(&p.bytes)),
        );
        m.insert("png_width".into(), json!(p.width));
        m.insert("png_height".into(), json!(p.height));
    }
    for f in files {
        let key = if f.path.to_ascii_lowercase().ends_with(".svg") {
            "svg_file"
        } else {
            "png_file"
        };
        m.insert(key.into(), json!(f.path));
    }
    Value::Object(m)
}

/// How a result is shaped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A tool result: `response` and image `parts`.
    Tool,
    /// A graph record of plain fields.
    Record,
}

/// Smallest PNG scale the budget fallback goes down to.
const MIN_SCALE: f64 = 0.25;
/// Redraws the budget fallback makes at most.
const MAX_SHRINKS: u32 = 8;

/// The scale of the next, smaller PNG; `None` once the floor or the number of
/// redraws is reached.
fn next_scale(scale: f64, tries: u32) -> Option<f64> {
    (scale > MIN_SCALE && tries < MAX_SHRINKS).then(|| (scale * 0.7).max(MIN_SCALE))
}

/// Builds the result JSON, shrinking it to the budget: first the SVG text is
/// left out, then the PNG is drawn smaller. Every step is a warning.
pub fn deliver(r: Rendered, output: Output, files: &[Written]) -> Res<String> {
    deliver_as(r, output, files, Shape::Tool)
}

/// [`deliver`] in a chosen shape.
pub fn deliver_as(mut r: Rendered, output: Output, files: &[Written], shape: Shape) -> Res<String> {
    let mut warnings = std::mem::take(&mut r.warnings);
    let mut with_svg = true;
    let mut scale = r.scale;
    let mut tries = 0;
    loop {
        let doc = match shape {
            Shape::Tool => assemble(&r, output, with_svg, files, &warnings),
            Shape::Record => record(&r, output, with_svg, files, &warnings),
        };
        let text = serde_json::to_string(&doc)
            .map_err(|e| format!("the result could not be written: {e}"))?;
        let budget = match shape {
            Shape::Tool => BUDGET,
            Shape::Record => RECORD_BUDGET,
        };
        if text.len() <= budget {
            return Ok(text);
        }
        if with_svg && output != Output::Png {
            with_svg = false;
            warnings.push("the SVG is left out of the result because it would exceed the output limit; the PNG and any saved files are complete".into());
            continue;
        }
        let next = next_scale(scale, tries).filter(|_| r.png.is_some());
        let Some(next) = next else {
            return fail(
                "the chart is too detailed for the output limit; reduce the data, the size or the number of series",
            );
        };
        scale = next;
        tries += 1;
        r.png = Some(raster::render(&r.svg, scale)?);
        let note = format!("the PNG was drawn at {scale:.2}x so the result fits the output limit");
        warnings.retain(|w| !w.starts_with("the PNG was drawn at "));
        warnings.push(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(svg_len: usize) -> Rendered {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\" height=\"150\"><rect width=\"200\" height=\"150\" fill=\"#fff\"/><!--{}--></svg>",
            "x".repeat(svg_len)
        );
        let png = raster::render(&svg, 1.0).unwrap();
        Rendered {
            chart: "line",
            svg,
            png: Some(png),
            scale: 1.0,
            width: 200,
            height: 150,
            alt: "a chart".into(),
            series: vec![SeriesInfo {
                name: "a".into(),
                mark: "line",
                color: "#000000".into(),
                axis: "left",
                points: 3,
                min: Some(0.1 + 0.2),
                max: Some(2.0),
                ..SeriesInfo::default()
            }],
            warnings: vec!["w".into()],
            plot: crate::frame::Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
        }
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn a_result_carries_the_svg_the_series_the_warnings_and_one_image_part() {
        let v = parse(&deliver(rendered(10), Output::Both, &[]).unwrap());
        assert_eq!(v["response"]["chart"], "line");
        assert_eq!(
            (
                v["response"]["width"].as_u64(),
                v["response"]["height"].as_u64()
            ),
            (Some(200), Some(150))
        );
        assert!(v["response"]["svg"].as_str().unwrap().starts_with("<svg"));
        assert_eq!(v["response"]["warnings"], json!(["w"]));
        assert_eq!(v["response"]["png"]["width"], 200);
        let parts = v["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(
            (parts[0]["type"].as_str(), parts[0]["mimeType"].as_str()),
            (Some("image"), Some("image/png"))
        );
        let png = base64::engine::general_purpose::STANDARD
            .decode(parts[0]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn series_numbers_are_rounded_to_stable_decimals_and_zero_gaps_are_kept() {
        let v = parse(&deliver(rendered(1), Output::Both, &[]).unwrap());
        let s = &v["response"]["series"][0];
        assert_eq!(s["min"], json!(0.3));
        assert_eq!(s["gaps"], 0);
        assert!(s.get("drawn").is_none());
    }

    #[test]
    fn svg_only_has_no_image_part_and_png_only_has_no_svg_text() {
        let v = parse(
            &deliver(
                {
                    let mut r = rendered(1);
                    r.png = None;
                    r
                },
                Output::Svg,
                &[],
            )
            .unwrap(),
        );
        assert!(
            v.get("parts").is_none()
                && v["response"]["svg"].is_string()
                && v["response"].get("png").is_none()
        );
        let v = parse(&deliver(rendered(1), Output::Png, &[]).unwrap());
        assert!(v["response"].get("svg").is_none() && v["parts"].is_array());
        assert!(
            v["response"]["warnings"].as_array().unwrap().len() == 1,
            "dropping the svg on request is not a warning"
        );
    }

    #[test]
    fn written_files_are_listed() {
        let files = [Written {
            path: "out/chart.svg".into(),
            bytes: 42,
        }];
        let v = parse(&deliver(rendered(1), Output::Both, &files).unwrap());
        assert_eq!(
            v["response"]["files"],
            json!([{"path": "out/chart.svg", "bytes": 42}])
        );
    }

    #[test]
    fn an_svg_over_the_budget_is_left_out_with_a_warning_and_the_png_stays() {
        let v = parse(&deliver(rendered(BUDGET + 1000), Output::Both, &[]).unwrap());
        assert!(v["response"].get("svg").is_none());
        assert!(
            v["response"]["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("SVG is left out"))
        );
        assert!(v["parts"].is_array());
    }

    #[test]
    fn a_result_that_cannot_fit_even_without_the_svg_is_an_error() {
        let mut r = rendered(1);
        r.alt = "y".repeat(BUDGET + 10);
        let e = deliver(r, Output::Both, &[]).unwrap_err().0;
        assert!(e.contains("too detailed for the output limit"), "{e}");
    }

    #[test]
    fn a_png_over_the_budget_is_drawn_again_at_a_smaller_scale_with_a_warning() {
        let mut r = rendered(1);
        // Random bytes stand in for a picture too detailed to fit: they cannot be compressed.
        let mut x = 0x9E37_79B9_7F4A_7C15_u64;
        let noise: Vec<u8> = (0..3_000_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        r.png = Some(Png {
            bytes: noise,
            width: 200,
            height: 150,
        });
        let text = deliver(r, Output::Both, &[]).unwrap();
        assert!(text.len() <= BUDGET);
        let v = parse(&text);
        let w = v["response"]["warnings"].as_array().unwrap();
        assert!(
            w.iter().any(|w| w
                .as_str()
                .unwrap()
                .starts_with("the PNG was drawn at 0.70x")),
            "{w:?}"
        );
        assert!(
            w.iter()
                .any(|w| w.as_str().unwrap().contains("SVG is left out")),
            "the SVG goes first"
        );
        let png = base64::engine::general_purpose::STANDARD
            .decode(v["parts"][0]["data"].as_str().unwrap())
            .unwrap();
        let (width, _, _) = raster::decode(&png).unwrap();
        assert_eq!(width, 140, "200 pixels at 0.7");
    }

    #[test]
    fn the_png_scale_steps_down_by_thirty_percent_to_a_floor_of_a_quarter() {
        let mut seen = Vec::new();
        let (mut scale, mut tries) = (1.0, 0);
        while let Some(next) = next_scale(scale, tries) {
            seen.push(next);
            scale = next;
            tries += 1;
        }
        let want = [0.7, 0.49, 0.343, 0.2401_f64.max(0.25)];
        assert_eq!(seen.len(), want.len(), "{seen:?}");
        for (got, want) in seen.iter().zip(want) {
            assert!((got - want).abs() < 1e-12, "{seen:?}");
        }
        // Every step below one half is still taken: only the floor and the redraw count stop it.
        assert!(next_scale(0.49, 2).is_some() && next_scale(0.343, 3).is_some());
        assert!(next_scale(0.25, 3).is_none());
        assert!(
            next_scale(1.0, MAX_SHRINKS).is_none() && next_scale(1.0, MAX_SHRINKS - 1).is_some()
        );
    }

    #[test]
    fn a_result_of_exactly_the_budget_is_kept_and_one_byte_more_loses_the_svg() {
        let base = deliver(rendered(0), Output::Both, &[]).unwrap().len();
        let exact = deliver(rendered(BUDGET - base), Output::Both, &[]).unwrap();
        assert_eq!(exact.len(), BUDGET);
        assert!(parse(&exact)["response"]["svg"].is_string());
        let over = deliver(rendered(BUDGET - base + 1), Output::Both, &[]).unwrap();
        assert!(over.len() < BUDGET);
        let v = parse(&over);
        assert!(v["response"].get("svg").is_none());
        assert!(
            v["response"]["warnings"]
                .to_string()
                .contains("SVG is left out")
        );
    }

    #[test]
    fn a_graph_record_is_held_to_the_smaller_budget_with_the_same_fallbacks() {
        let r = rendered(RECORD_BUDGET + 10);
        let text = deliver_as(r, Output::Both, &[], Shape::Record).unwrap();
        assert!(text.len() <= RECORD_BUDGET, "{} bytes", text.len());
        let v = parse(&text);
        assert!(v.get("svg").is_none(), "the SVG text is left out first");
        assert!(v["png_base64"].is_string());
        assert!(v["warnings"].to_string().contains("SVG is left out"));
        let same = rendered(RECORD_BUDGET + 10);
        assert!(
            deliver(same, Output::Both, &[]).is_ok_and(|t| t.contains("\"svg\"")),
            "a tool result of that size keeps its SVG"
        );
    }

    #[test]
    fn the_whole_result_always_stays_within_the_budget() {
        for extra in [0, BUDGET / 2, BUDGET + 5] {
            let text = deliver(rendered(extra), Output::Both, &[]).unwrap();
            assert!(text.len() <= BUDGET, "{} bytes", text.len());
        }
    }
}
