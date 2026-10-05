//! Text measurement, cleaning and XML escaping against the one embedded font.
//!
//! Widths are advance sums of Liberation Sans (metric-compatible with Arial
//! and Helvetica), so a viewer that falls back to either lays text out the
//! same way the PNG does. Characters the font lacks are measured as the
//! font's `.notdef` box and reported by [`missing_glyphs`].

use std::cell::OnceCell;
use std::collections::BTreeSet;

use ttf_parser::{Face, GlyphId};

/// Regular weight, embedded.
pub const REGULAR: &[u8] = include_bytes!("../fonts/LiberationSans-Regular.ttf");
/// Bold weight, embedded.
pub const BOLD: &[u8] = include_bytes!("../fonts/LiberationSans-Bold.ttf");
/// The family name both faces carry.
pub const FAMILY: &str = "Liberation Sans";
/// The CSS font stack written into every SVG.
pub const STACK: &str = "Liberation Sans, Arial, Helvetica, sans-serif";

struct Faces {
    regular: Face<'static>,
    bold: Face<'static>,
}

thread_local! {
    static FACES: OnceCell<Option<Faces>> = const { OnceCell::new() };
}

fn with_faces<R>(f: impl FnOnce(Option<&Faces>) -> R) -> R {
    FACES.with(|cell| {
        let faces = cell.get_or_init(|| {
            Some(Faces {
                regular: Face::parse(REGULAR, 0).ok()?,
                bold: Face::parse(BOLD, 0).ok()?,
            })
        });
        f(faces.as_ref())
    })
}

fn advance(face: &Face<'_>, c: char) -> f64 {
    let upem = f64::from(face.units_per_em().max(1));
    let gid = face.glyph_index(c).unwrap_or(GlyphId(0));
    f64::from(face.glyph_hor_advance(gid).unwrap_or(0)) / upem
}

/// Width in pixels of `text` at `size` pixels. Zero-width and control
/// characters count as nothing; a missing glyph counts as the `.notdef` box.
pub fn width(text: &str, size: f64, bold: bool) -> f64 {
    with_faces(|faces| {
        let Some(faces) = faces else {
            return text.chars().count() as f64 * size * 0.55;
        };
        let face = if bold { &faces.bold } else { &faces.regular };
        text.chars()
            .filter(|c| !c.is_control())
            .map(|c| advance(face, c))
            .sum::<f64>()
            * size
    })
}

/// The characters of `text` the font has no glyph for, sorted, without
/// duplicates. Whitespace and control characters never count.
pub fn missing_glyphs(text: &str) -> BTreeSet<char> {
    with_faces(|faces| {
        let Some(faces) = faces else {
            return BTreeSet::new();
        };
        text.chars()
            .filter(|c| !c.is_whitespace() && !c.is_control())
            .filter(|c| faces.regular.glyph_index(*c).is_none())
            .collect()
    })
}

/// Replaces line breaks and tabs by spaces and drops the other control
/// characters and the code points XML 1.0 cannot carry, so the text can sit
/// in one `<text>` element.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            '\u{FFFE}' | '\u{FFFF}' => None,
            c => Some(c),
        })
        .collect()
}

/// Escapes `text` for element content and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Shortens `text` with a trailing ellipsis until it fits `max_width`.
/// Returns the text to draw and whether it was shortened.
pub fn elide(text: &str, max_width: f64, size: f64, bold: bool) -> (String, bool) {
    if width(text, size, bold) <= max_width {
        return (text.to_owned(), false);
    }
    let ellipsis = "\u{2026}";
    let budget = max_width - width(ellipsis, size, bold);
    let mut out = String::new();
    let mut used = 0.0;
    for c in text.chars() {
        let w = width(c.encode_utf8(&mut [0; 4]), size, bold);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push_str(ellipsis);
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_scales_with_size_and_grows_with_text() {
        let a = width("Hello", 10.0, false);
        assert!(a > 0.0);
        assert!((width("Hello", 20.0, false) - 2.0 * a).abs() < 1e-9);
        assert!(width("Hello world", 10.0, false) > a);
        assert_eq!(width("", 10.0, false), 0.0);
    }

    #[test]
    fn bold_is_wider_than_regular_for_the_same_text() {
        assert!(width("Quarterly revenue", 12.0, true) > width("Quarterly revenue", 12.0, false));
    }

    #[test]
    fn digits_are_tabular_in_the_font() {
        let w = width("0", 10.0, false);
        for d in "123456789".chars() {
            assert!((width(&d.to_string(), 10.0, false) - w).abs() < 1e-9);
        }
    }

    #[test]
    fn a_known_advance_matches_the_font_metrics() {
        // Liberation Sans advance of "0" is 1139 of 2048 units.
        assert!((width("0", 2048.0, false) - 1139.0).abs() < 1e-6);
    }

    #[test]
    fn control_characters_have_no_width() {
        assert_eq!(width("\u{7}\n", 10.0, false), 0.0);
    }

    #[test]
    fn latin_greek_and_cyrillic_have_glyphs_and_cjk_and_emoji_do_not() {
        assert!(missing_glyphs("Abc \u{3b1}\u{3b2} \u{43f}\u{440}").is_empty());
        let m = missing_glyphs("a\u{6c49}\u{5b57}b\u{1F600}\u{6c49}");
        assert_eq!(m.len(), 3);
        assert!(m.contains(&'\u{6c49}') && m.contains(&'\u{1F600}'));
    }

    #[test]
    fn missing_glyphs_ignore_whitespace_and_controls() {
        assert!(missing_glyphs(" \t\n\u{7}").is_empty());
    }

    #[test]
    fn clean_turns_breaks_into_spaces_and_drops_controls() {
        assert_eq!(clean("a\nb\r\nc\td"), "a b  c d");
        assert_eq!(clean("a\u{0}b\u{1b}c\u{FFFE}d"), "abcd");
    }

    #[test]
    fn escape_neutralises_every_markup_character() {
        assert_eq!(
            escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("</text><script>"), "&lt;/text&gt;&lt;script&gt;");
    }

    #[test]
    fn elide_leaves_text_that_fits_and_marks_text_that_does_not() {
        let (t, cut) = elide("short", 200.0, 12.0, false);
        assert_eq!((t.as_str(), cut), ("short", false));
        let long = "a very long category label that cannot possibly fit in the space";
        let (t, cut) = elide(long, 90.0, 12.0, false);
        assert!(cut);
        assert!(t.ends_with('\u{2026}'));
        assert!(width(&t, 12.0, false) <= 90.0);
        assert!(long.starts_with(t.trim_end_matches('\u{2026}')));
    }

    #[test]
    fn elide_to_nothing_still_returns_an_ellipsis_and_never_panics() {
        let (t, cut) = elide("anything", 1.0, 12.0, false);
        assert!(cut);
        assert_eq!(t, "\u{2026}");
        let (t, cut) = elide("", 0.0, 12.0, false);
        assert_eq!((t.as_str(), cut), ("", false));
    }

    #[test]
    fn elide_does_not_split_a_multibyte_character() {
        let (t, _) = elide(
            "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
            30.0,
            12.0,
            false,
        );
        assert!(t.chars().all(|c| c == '\u{e9}' || c == '\u{2026}'));
    }
}
