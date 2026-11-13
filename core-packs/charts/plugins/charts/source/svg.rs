//! A small SVG writer. Every number goes through [`crate::num::push_coord`]
//! and every string through [`crate::text::escape`], so the output is
//! byte-identical on every platform and no caller text can add markup.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::num::push_coord;
use crate::text::{STACK, clean, escape, limit_chars, missing_glyphs};

/// Longest tooltip or title text written into a picture, in characters.
const TIP_MAX: usize = 300;
/// Longest description written into a picture, in characters.
const DESC_MAX: usize = 2000;

/// Paint for one element. Unset fields are left out of the markup.
#[derive(Debug, Clone, Default)]
pub struct Style<'a> {
    /// Fill colour, or `none`.
    pub fill: Option<&'a str>,
    /// Stroke colour.
    pub stroke: Option<&'a str>,
    /// Stroke width in pixels; written only with a stroke.
    pub stroke_width: f64,
    /// Fill opacity, 0 to 1.
    pub fill_opacity: Option<f64>,
    /// Stroke opacity, 0 to 1.
    pub stroke_opacity: Option<f64>,
    /// Dash pattern such as `4 3`.
    pub dash: Option<&'a str>,
    /// Round joins and caps for lines.
    pub round: bool,
}

impl<'a> Style<'a> {
    /// A filled shape.
    pub fn fill(color: &'a str) -> Self {
        Self {
            fill: Some(color),
            ..Self::default()
        }
    }

    /// A stroked outline with no fill.
    pub fn stroke(color: &'a str, width: f64) -> Self {
        Self {
            fill: Some("none"),
            stroke: Some(color),
            stroke_width: width,
            ..Self::default()
        }
    }

    /// The same style with a fill opacity.
    pub fn fill_alpha(mut self, alpha: f64) -> Self {
        self.fill_opacity = Some(alpha);
        self
    }

    /// The same style with rounded joins.
    pub fn rounded(mut self) -> Self {
        self.round = true;
        self
    }

    /// The same style with a dash pattern.
    pub fn dashed(mut self, pattern: &'a str) -> Self {
        self.dash = Some(pattern);
        self
    }

    /// The same style with an added stroke.
    pub fn outlined(mut self, color: &'a str, width: f64) -> Self {
        self.stroke = Some(color);
        self.stroke_width = width;
        self
    }
}

/// Horizontal text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Text starts at x.
    Start,
    /// Text is centred on x.
    Middle,
    /// Text ends at x.
    End,
}

/// How one text is drawn.
#[derive(Debug, Clone)]
pub struct TextStyle<'a> {
    /// Font size in pixels.
    pub size: f64,
    /// Bold weight.
    pub bold: bool,
    /// Alignment about x.
    pub anchor: Anchor,
    /// Fill colour.
    pub fill: &'a str,
    /// Clockwise rotation in degrees about (x, y).
    pub rotate: f64,
    /// Full text shown as a tooltip when the drawn text was shortened.
    pub full: Option<&'a str>,
}

impl<'a> TextStyle<'a> {
    /// Regular text of `size` in `fill`, starting at x.
    pub fn new(size: f64, fill: &'a str) -> Self {
        Self {
            size,
            bold: false,
            anchor: Anchor::Start,
            fill,
            rotate: 0.0,
            full: None,
        }
    }

    /// The same style, bold.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// The same style with an alignment.
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// The same style rotated by `degrees`.
    pub fn rotate(mut self, degrees: f64) -> Self {
        self.rotate = degrees;
        self
    }

    /// The same style with a tooltip carrying the untruncated text.
    pub fn full(mut self, full: &'a str) -> Self {
        self.full = Some(full);
        self
    }
}

/// A path under construction.
#[derive(Debug, Default, Clone)]
pub struct PathData {
    d: String,
}

impl PathData {
    /// An empty path.
    pub fn new() -> Self {
        Self::default()
    }

    fn cmd(&mut self, c: char, xs: &[f64]) {
        if !self.d.is_empty() {
            self.d.push(' ');
        }
        self.d.push(c);
        for (i, v) in xs.iter().enumerate() {
            self.d.push(if i == 0 { ' ' } else { ',' });
            push_coord(&mut self.d, *v);
        }
    }

    /// Starts a sub-path at (x, y).
    pub fn move_to(&mut self, x: f64, y: f64) -> &mut Self {
        self.cmd('M', &[x, y]);
        self
    }

    /// Straight line to (x, y).
    pub fn line_to(&mut self, x: f64, y: f64) -> &mut Self {
        self.cmd('L', &[x, y]);
        self
    }

    /// Circular arc of radius `r` to (x, y).
    pub fn arc_to(&mut self, r: f64, large: bool, sweep: bool, x: f64, y: f64) -> &mut Self {
        if !self.d.is_empty() {
            self.d.push(' ');
        }
        self.d.push_str("A ");
        push_coord(&mut self.d, r);
        self.d.push(',');
        push_coord(&mut self.d, r);
        let _ = write!(self.d, " 0 {} {} ", u8::from(large), u8::from(sweep));
        push_coord(&mut self.d, x);
        self.d.push(',');
        push_coord(&mut self.d, y);
        self
    }

    /// Closes the sub-path.
    pub fn close(&mut self) -> &mut Self {
        if !self.d.is_empty() {
            self.d.push(' ');
        }
        self.d.push('Z');
        self
    }

    /// True when nothing was added.
    pub fn is_empty(&self) -> bool {
        self.d.is_empty()
    }

    /// The `d` attribute value.
    pub fn as_str(&self) -> &str {
        &self.d
    }
}

/// The document being written. The body is collected first; the root element
/// with its accessible title and description is added by [`Svg::finish`],
/// once the description is known.
#[derive(Debug)]
pub struct Svg {
    buf: String,
    width: f64,
    height: f64,
    background: String,
    missing: BTreeSet<char>,
    clips: usize,
}

fn paint(buf: &mut String, st: &Style<'_>) {
    if let Some(fill) = st.fill {
        let _ = write!(buf, " fill=\"{fill}\"");
    }
    if let Some(o) = st.fill_opacity {
        buf.push_str(" fill-opacity=\"");
        push_coord(buf, o);
        buf.push('"');
    }
    if let Some(stroke) = st.stroke {
        let _ = write!(buf, " stroke=\"{stroke}\" stroke-width=\"");
        push_coord(buf, st.stroke_width);
        buf.push('"');
        if let Some(o) = st.stroke_opacity {
            buf.push_str(" stroke-opacity=\"");
            push_coord(buf, o);
            buf.push('"');
        }
        if let Some(d) = st.dash {
            let _ = write!(buf, " stroke-dasharray=\"{d}\"");
        }
        if st.round {
            buf.push_str(" stroke-linejoin=\"round\" stroke-linecap=\"round\"");
        }
    }
}

fn attr(buf: &mut String, name: &str, v: f64) {
    let _ = write!(buf, " {name}=\"");
    push_coord(buf, v);
    buf.push('"');
}

impl Svg {
    /// Starts a `width` by `height` document on a `background` fill.
    pub fn new(width: f64, height: f64, background: &str) -> Self {
        Self {
            buf: String::with_capacity(16 * 1024),
            width,
            height,
            background: background.to_owned(),
            missing: BTreeSet::new(),
            clips: 0,
        }
    }

    /// A rectangle.
    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, st: &Style<'_>) {
        self.buf.push_str("<rect");
        attr(&mut self.buf, "x", x);
        attr(&mut self.buf, "y", y);
        attr(&mut self.buf, "width", w);
        attr(&mut self.buf, "height", h);
        paint(&mut self.buf, st);
        self.buf.push_str("/>");
    }

    /// A straight line.
    pub fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, st: &Style<'_>) {
        self.buf.push_str("<line");
        attr(&mut self.buf, "x1", x1);
        attr(&mut self.buf, "y1", y1);
        attr(&mut self.buf, "x2", x2);
        attr(&mut self.buf, "y2", y2);
        paint(&mut self.buf, st);
        self.buf.push_str("/>");
    }

    /// A path from `d`.
    pub fn path(&mut self, d: &PathData, st: &Style<'_>) {
        if d.is_empty() {
            return;
        }
        let _ = write!(self.buf, "<path d=\"{}\"", d.as_str());
        paint(&mut self.buf, st);
        self.buf.push_str("/>");
    }

    /// A circle.
    pub fn circle(&mut self, cx: f64, cy: f64, r: f64, st: &Style<'_>) {
        self.buf.push_str("<circle");
        attr(&mut self.buf, "cx", cx);
        attr(&mut self.buf, "cy", cy);
        attr(&mut self.buf, "r", r);
        paint(&mut self.buf, st);
        self.buf.push_str("/>");
    }

    /// A circle that carries a `<title>` tooltip.
    pub fn circle_titled(&mut self, cx: f64, cy: f64, r: f64, st: &Style<'_>, title: &str) {
        self.buf.push_str("<circle");
        attr(&mut self.buf, "cx", cx);
        attr(&mut self.buf, "cy", cy);
        attr(&mut self.buf, "r", r);
        paint(&mut self.buf, st);
        let _ = write!(
            self.buf,
            "><title>{}</title></circle>",
            escape(&clean(&limit_chars(title, TIP_MAX)))
        );
    }

    /// A rectangle that carries a `<title>` tooltip.
    pub fn rect_titled(&mut self, x: f64, y: f64, w: f64, h: f64, st: &Style<'_>, title: &str) {
        self.buf.push_str("<rect");
        attr(&mut self.buf, "x", x);
        attr(&mut self.buf, "y", y);
        attr(&mut self.buf, "width", w);
        attr(&mut self.buf, "height", h);
        paint(&mut self.buf, st);
        let _ = write!(
            self.buf,
            "><title>{}</title></rect>",
            escape(&clean(&limit_chars(title, TIP_MAX)))
        );
    }

    /// A path that carries a `<title>` tooltip.
    pub fn path_titled(&mut self, d: &PathData, st: &Style<'_>, title: &str) {
        if d.is_empty() {
            return;
        }
        let _ = write!(self.buf, "<path d=\"{}\"", d.as_str());
        paint(&mut self.buf, st);
        let _ = write!(
            self.buf,
            "><title>{}</title></path>",
            escape(&clean(&limit_chars(title, TIP_MAX)))
        );
    }

    /// Text anchored at (x, y), where y is the baseline.
    pub fn text(&mut self, x: f64, y: f64, text: &str, st: &TextStyle<'_>) {
        let text = clean(text);
        if text.is_empty() {
            return;
        }
        self.missing.extend(missing_glyphs(&text));
        let grouped = st.full.is_some();
        if let Some(full) = st.full {
            self.missing
                .extend(missing_glyphs(&clean(&limit_chars(full, TIP_MAX))));
            let _ = write!(
                self.buf,
                "<g><title>{}</title>",
                escape(&clean(&limit_chars(full, TIP_MAX)))
            );
        }
        self.buf.push_str("<text");
        attr(&mut self.buf, "x", x);
        attr(&mut self.buf, "y", y);
        attr(&mut self.buf, "font-size", st.size);
        if st.bold {
            self.buf.push_str(" font-weight=\"bold\"");
        }
        match st.anchor {
            Anchor::Start => {}
            Anchor::Middle => self.buf.push_str(" text-anchor=\"middle\""),
            Anchor::End => self.buf.push_str(" text-anchor=\"end\""),
        }
        let _ = write!(self.buf, " fill=\"{}\"", st.fill);
        if st.rotate != 0.0 {
            self.buf.push_str(" transform=\"rotate(");
            push_coord(&mut self.buf, st.rotate);
            self.buf.push(' ');
            push_coord(&mut self.buf, x);
            self.buf.push(' ');
            push_coord(&mut self.buf, y);
            self.buf.push_str(")\"");
        }
        let _ = write!(self.buf, ">{}</text>", escape(&text));
        if grouped {
            self.buf.push_str("</g>");
        }
    }

    /// Opens a group with raw, already-safe attributes.
    pub fn open_group(&mut self, attrs: &str) {
        let _ = write!(
            self.buf,
            "<g{}{attrs}>",
            if attrs.is_empty() { "" } else { " " }
        );
    }

    /// Closes the last group.
    pub fn close_group(&mut self) {
        self.buf.push_str("</g>");
    }

    /// Defines a rectangular clip and returns the attribute that applies it.
    pub fn clip_rect(&mut self, x: f64, y: f64, w: f64, h: f64) -> String {
        self.clips += 1;
        let id = format!("clip{}", self.clips);
        let _ = write!(self.buf, "<clipPath id=\"{id}\"><rect");
        attr(&mut self.buf, "x", x);
        attr(&mut self.buf, "y", y);
        attr(&mut self.buf, "width", w);
        attr(&mut self.buf, "height", h);
        self.buf.push_str("/></clipPath>");
        format!("clip-path=\"url(#{id})\"")
    }

    /// Defines a linear gradient between `from` and `to` stops along y.
    pub fn vertical_gradient(&mut self, id: &str, stops: &[(f64, String)]) {
        let _ = write!(
            self.buf,
            "<linearGradient id=\"{id}\" x1=\"0\" y1=\"1\" x2=\"0\" y2=\"0\">"
        );
        for (offset, color) in stops {
            self.buf.push_str("<stop");
            attr(&mut self.buf, "offset", *offset);
            let _ = write!(self.buf, " stop-color=\"{color}\"/>");
        }
        self.buf.push_str("</linearGradient>");
    }

    /// Characters drawn that the font lacks.
    pub fn missing(&self) -> &BTreeSet<char> {
        &self.missing
    }

    /// The finished document with `title` and `desc` as its accessible name
    /// and description, and the characters drawn that the font lacks.
    pub fn finish(self, title: &str, desc: &str) -> (String, BTreeSet<char>) {
        let mut out = String::with_capacity(self.buf.len() + 512);
        out.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\"");
        attr(&mut out, "width", self.width);
        attr(&mut out, "height", self.height);
        out.push_str(" viewBox=\"0 0 ");
        push_coord(&mut out, self.width);
        out.push(' ');
        push_coord(&mut out, self.height);
        let _ = write!(
            out,
            "\" role=\"img\" aria-labelledby=\"chart-title chart-desc\" font-family=\"{STACK}\">"
        );
        let _ = write!(
            out,
            "<title id=\"chart-title\">{}</title><desc id=\"chart-desc\">{}</desc>",
            escape(&clean(&limit_chars(title, TIP_MAX))),
            escape(&clean(&limit_chars(desc, DESC_MAX)))
        );
        out.push_str("<rect");
        attr(&mut out, "x", 0.0);
        attr(&mut out, "y", 0.0);
        attr(&mut out, "width", self.width);
        attr(&mut out, "height", self.height);
        let _ = write!(out, " fill=\"{}\"/>", self.background);
        out.push_str(&self.buf);
        out.push_str("</svg>");
        (out, self.missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(f: impl FnOnce(&mut Svg)) -> String {
        let mut s = Svg::new(100.0, 50.0, "#fff");
        f(&mut s);
        s.finish("T", "D").0
    }

    #[test]
    fn the_root_carries_size_accessibility_and_a_background() {
        let s = doc(|_| {});
        assert!(s.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"50\" viewBox=\"0 0 100 50\" role=\"img\""));
        assert!(s.contains("<title id=\"chart-title\">T</title><desc id=\"chart-desc\">D</desc>"));
        assert!(s.contains("<rect x=\"0\" y=\"0\" width=\"100\" height=\"50\" fill=\"#fff\"/>"));
        assert!(s.ends_with("</svg>"));
    }

    #[test]
    fn shapes_write_fixed_attribute_order_and_trimmed_numbers() {
        let s = doc(|s| {
            s.line(
                1.0,
                2.5,
                3.0,
                4.0,
                &Style::stroke("#000", 1.5).dashed("4 3"),
            );
            s.circle(5.0, 6.0, 2.0, &Style::fill("red").fill_alpha(0.5));
        });
        assert!(s.contains(
            "<line x1=\"1\" y1=\"2.5\" x2=\"3\" y2=\"4\" fill=\"none\" stroke=\"#000\" stroke-width=\"1.5\" stroke-dasharray=\"4 3\"/>"
        ));
        assert!(
            s.contains("<circle cx=\"5\" cy=\"6\" r=\"2\" fill=\"red\" fill-opacity=\"0.5\"/>")
        );
    }

    #[test]
    fn paths_join_commands_and_skip_when_empty() {
        let mut p = PathData::new();
        assert!(p.is_empty());
        p.move_to(0.0, 0.0)
            .line_to(10.0, 5.5)
            .arc_to(3.0, true, false, 1.0, 2.0)
            .close();
        assert_eq!(p.as_str(), "M 0,0 L 10,5.5 A 3,3 0 1 0 1,2 Z");
        let s = doc(|s| {
            s.path(&PathData::new(), &Style::fill("red"));
            s.path(&p, &Style::fill("red"));
        });
        assert_eq!(s.matches("<path").count(), 1);
    }

    #[test]
    fn text_is_escaped_and_cannot_inject_markup() {
        let s = doc(|s| {
            s.text(
                1.0,
                2.0,
                "</text><script>alert(1)</script>&\"'",
                &TextStyle::new(12.0, "#000"),
            );
        });
        assert!(!s.contains("<script"));
        assert_eq!(s.matches("</text>").count(), 1);
        assert!(s.contains("&lt;/text&gt;&lt;script&gt;alert(1)&lt;/script&gt;&amp;&quot;&#39;"));
    }

    #[test]
    fn titles_and_descriptions_are_escaped_too() {
        let s = Svg::new(10.0, 10.0, "#fff")
            .finish("</title><x/>", "a & b")
            .0;
        assert!(s.contains("&lt;/title&gt;&lt;x/&gt;"));
        assert!(s.contains("a &amp; b"));
        assert_eq!(s.matches("<title").count(), 1);
    }

    #[test]
    fn a_shortened_text_carries_its_full_text_as_a_tooltip() {
        let s = doc(|s| {
            s.text(
                5.0,
                6.0,
                "Long\u{2026}",
                &TextStyle::new(11.0, "#111")
                    .anchor(Anchor::End)
                    .full("Long label <b>"),
            );
        });
        assert!(s.contains("<g><title>Long label &lt;b&gt;</title><text x=\"5\" y=\"6\" font-size=\"11\" text-anchor=\"end\" fill=\"#111\">Long\u{2026}</text></g>"));
    }

    #[test]
    fn rotation_is_about_the_anchor_point() {
        let s = doc(|s| s.text(10.0, 20.0, "r", &TextStyle::new(10.0, "#000").rotate(-45.0)));
        assert!(s.contains("transform=\"rotate(-45 10 20)\""));
    }

    #[test]
    fn empty_text_writes_nothing_and_controls_are_stripped() {
        let s = doc(|s| {
            s.text(0.0, 0.0, "\u{0}\u{1}", &TextStyle::new(10.0, "#000"));
            s.text(0.0, 0.0, "a\u{0}b", &TextStyle::new(10.0, "#000"));
        });
        assert_eq!(s.matches("<text").count(), 1);
        assert!(s.contains(">ab</text>"));
    }

    #[test]
    fn missing_glyphs_are_collected_from_every_text() {
        let mut s = Svg::new(10.0, 10.0, "#fff");
        s.text(0.0, 0.0, "\u{6c49}", &TextStyle::new(10.0, "#000"));
        s.text(
            0.0,
            0.0,
            "a",
            &TextStyle::new(10.0, "#000").full("\u{1F600}"),
        );
        let (_, missing) = s.finish("t", "d");
        assert_eq!(missing.len(), 2);
    }

    #[test]
    fn tooltip_titles_are_escaped() {
        let s = doc(|s| {
            s.rect_titled(0.0, 0.0, 1.0, 1.0, &Style::fill("red"), "a<b");
            s.circle_titled(0.0, 0.0, 1.0, &Style::fill("red"), "c&d");
        });
        assert!(s.contains("<title>a&lt;b</title>"));
        assert!(s.contains("<title>c&amp;d</title>"));
    }

    #[test]
    fn clips_get_unique_ids_and_a_usable_attribute() {
        let mut s = Svg::new(10.0, 10.0, "#fff");
        let a = s.clip_rect(0.0, 0.0, 5.0, 5.0);
        let b = s.clip_rect(1.0, 1.0, 5.0, 5.0);
        assert_eq!(a, "clip-path=\"url(#clip1)\"");
        assert_eq!(b, "clip-path=\"url(#clip2)\"");
    }
}
