//! Helpers shared by the chart tests: render a request, read the SVG back as
//! XML, and find marks by tag and attribute.

use roxmltree::{Document, Node};
use serde_json::Value;

use crate::err::Res;
use crate::frame::Rect;
use crate::output::{Rendered, Written};
use crate::spec::Request;

/// Renders a request given as JSON text.
pub fn render(json: &str) -> Res<(Rendered, Vec<Written>)> {
    let req: Request =
        serde_json::from_str(json).map_err(|e| crate::err::ChartError(e.to_string()))?;
    crate::pipeline::render(&req)
}

/// Renders a request that must succeed.
pub fn ok(json: &str) -> Rendered {
    match render(json) {
        Ok((r, _)) => r,
        Err(e) => panic!("{json} failed: {e}"),
    }
}

/// The failure sentence of a request that must fail.
pub fn err(json: &str) -> String {
    match render(json) {
        Ok(_) => panic!("{json} should have failed"),
        Err(e) => e.0,
    }
}

/// A request with inline rows.
pub fn with_rows(chart: &str, extra: &str, columns: &[&str], rows: &[Value]) -> String {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!(",{extra}")
    };
    format!(
        r#"{{"chart":"{chart}"{extra},"data":{{"columns":{},"rows":{}}}}}"#,
        serde_json::to_string(columns).unwrap_or_default(),
        serde_json::to_string(rows).unwrap_or_default()
    )
}

/// The parsed SVG; panics with the text when it is not well-formed XML.
pub fn parse(svg: &str) -> Document<'_> {
    Document::parse(svg).unwrap_or_else(|e| panic!("the SVG is not well-formed XML: {e}\n{svg}"))
}

/// Every element called `tag`, in document order.
pub fn all<'a, 'i>(doc: &'a Document<'i>, tag: &str) -> Vec<Node<'a, 'i>> {
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag)
        .collect()
}

/// A numeric attribute.
pub fn num(n: Node<'_, '_>, name: &str) -> f64 {
    n.attribute(name)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{name} missing on {n:?}"))
}

/// The text of the `<title>` child of an element.
pub fn title(n: Node<'_, '_>) -> Option<String> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == "title")
        .map(|t| t.text().unwrap_or("").to_owned())
}

/// Elements called `tag` whose `<title>` child starts with `prefix`.
pub fn titled<'a, 'i>(doc: &'a Document<'i>, tag: &str, prefix: &str) -> Vec<Node<'a, 'i>> {
    all(doc, tag)
        .into_iter()
        .filter(|n| title(*n).is_some_and(|t| t.starts_with(prefix)))
        .collect()
}

/// The text content of every `<text>` element.
pub fn texts(doc: &Document<'_>) -> Vec<String> {
    all(doc, "text")
        .into_iter()
        .map(|n| n.text().unwrap_or("").to_owned())
        .collect()
}

/// True when the rectangle contains the point, allowing `eps` of slack.
pub fn inside(r: &Rect, x: f64, y: f64, eps: f64) -> bool {
    x >= r.x - eps && x <= r.right() + eps && y >= r.y - eps && y <= r.bottom() + eps
}

/// A fill colour as (r, g, b).
pub fn rgb(hex: &str) -> (u8, u8, u8) {
    crate::palette::parse_hex(hex).unwrap_or_else(|| panic!("{hex} is not a colour"))
}

/// The points of a path made of `M` and `L` commands.
pub fn path_points(d: &str) -> Vec<(f64, f64)> {
    d.split_whitespace()
        .filter(|t| t.contains(','))
        .filter_map(|t| {
            let (x, y) = t.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        })
        .collect()
}

/// The `d` attribute of every path whose stroke is `color`, as point lists.
pub fn stroked_paths(doc: &Document<'_>, color: &str) -> Vec<Vec<(f64, f64)>> {
    all(doc, "path")
        .into_iter()
        .filter(|p| p.attribute("stroke") == Some(color) && p.attribute("fill") == Some("none"))
        .map(|p| path_points(p.attribute("d").unwrap_or("")))
        .collect()
}

/// A line through two reference points: pixel as a function of value.
#[derive(Debug, Clone, Copy)]
pub struct Fit {
    v0: f64,
    p0: f64,
    slope: f64,
}

impl Fit {
    /// The pixel of `v`.
    pub fn px(&self, v: f64) -> f64 {
        self.p0 + (v - self.v0) * self.slope
    }
}

fn parse_label(t: &str) -> Option<f64> {
    t.replace(',', "").parse().ok()
}

/// The vertical scale read back from the y tick labels drawn left of the plot.
pub fn y_fit(doc: &Document<'_>, plot: &Rect) -> Fit {
    let pairs: Vec<(f64, f64)> = all(doc, "text")
        .into_iter()
        .filter(|n| n.attribute("text-anchor") == Some("end") && n.attribute("transform").is_none())
        .filter(|n| {
            num(*n, "x") < plot.x
                && num(*n, "y") >= plot.y - 8.0
                && num(*n, "y") <= plot.bottom() + 8.0
        })
        .filter_map(|n| Some((parse_label(n.text()?)?, num(n, "y") - 4.0)))
        .collect();
    fit(&pairs)
}

/// The horizontal scale read back from the x tick labels drawn below the plot.
pub fn x_fit(doc: &Document<'_>, plot: &Rect) -> Fit {
    let pairs: Vec<(f64, f64)> = all(doc, "text")
        .into_iter()
        .filter(|n| {
            n.attribute("text-anchor") == Some("middle") && n.attribute("transform").is_none()
        })
        .filter(|n| num(*n, "y") > plot.bottom())
        .filter_map(|n| Some((parse_label(n.text()?)?, num(n, "x"))))
        .collect();
    fit(&pairs)
}

fn fit(pairs: &[(f64, f64)]) -> Fit {
    assert!(
        pairs.len() >= 2,
        "need two numeric tick labels, found {pairs:?}"
    );
    let (a, b) = (pairs[0], pairs[pairs.len() - 1]);
    Fit {
        v0: a.0,
        p0: a.1,
        slope: (b.1 - a.1) / (b.0 - a.0),
    }
}

/// The scale of the right axis, read from the tick labels right of the plot.
pub fn y2_fit(doc: &Document<'_>, plot: &Rect) -> Fit {
    let pairs: Vec<(f64, f64)> = all(doc, "text")
        .into_iter()
        .filter(|n| n.attribute("text-anchor").is_none() && n.attribute("transform").is_none())
        .filter(|n| {
            num(*n, "x") > plot.right()
                && num(*n, "y") >= plot.y - 8.0
                && num(*n, "y") <= plot.bottom() + 8.0
        })
        .filter_map(|n| Some((parse_label(n.text()?)?, num(n, "y") - 4.0)))
        .collect();
    fit(&pairs)
}

/// A rectangle element as (x, y, width, height).
pub fn rect_of(n: Node<'_, '_>) -> (f64, f64, f64, f64) {
    (num(n, "x"), num(n, "y"), num(n, "width"), num(n, "height"))
}
