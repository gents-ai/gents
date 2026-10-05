//! Hostile and foreign text: markup, control characters, huge strings and
//! scripts the font does not cover.

use base64::Engine as _;
use serde_json::{Value, json};

use crate::raster;
use crate::testkit::*;

const ALLOWED: [&str; 12] = [
    "svg",
    "title",
    "desc",
    "rect",
    "g",
    "line",
    "path",
    "circle",
    "text",
    "clipPath",
    "linearGradient",
    "stop",
];

fn assert_only_drawing_elements(doc: &roxmltree::Document<'_>) {
    for n in doc.descendants().filter(|n| n.is_element()) {
        assert!(
            ALLOWED.contains(&n.tag_name().name()),
            "unexpected element <{}>",
            n.tag_name().name()
        );
        for a in n.attributes() {
            assert!(
                !a.name().starts_with("on"),
                "event handler attribute {}",
                a.name()
            );
        }
    }
}

const HOSTILE: [&str; 9] = [
    "</text><script>alert(1)</script>",
    "<svg onload=alert(1)>",
    "\"><rect width='9999' height='9999'/>",
    "a & b &amp; c &#x41; &lt;",
    "]]> <!-- comment --> <![CDATA[x]]>",
    "'; DROP TABLE charts; --",
    "<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY e SYSTEM \"file:///etc/passwd\">]>&e;",
    "<a xlink:href=\"javascript:alert(1)\">x</a>",
    "</title></svg><svg>",
];

#[test]
fn markup_in_every_text_field_comes_out_as_plain_text() {
    for evil in HOSTILE {
        let req = json!({
            "chart": "bar",
            "title": evil,
            "subtitle": evil,
            "x_label": evil,
            "y_label": evil,
            "data": {"columns": ["k", evil], "rows": [[evil, 1], ["plain", 2]]},
        });
        let r = ok(&req.to_string());
        let doc = parse(&r.svg);
        assert_only_drawing_elements(&doc);
        let t = texts(&doc);
        assert!(
            t.iter().any(|x| x == evil),
            "the text survives intact as text: {evil:?} in {t:?}"
        );
        assert_eq!(all(&doc, "svg").len(), 1, "no second svg: {evil:?}");
        assert!(all(&doc, "script").is_empty());
        assert_eq!(doc.root_element().tag_name().name(), "svg");
        // The accessible name and description carry it escaped too.
        assert_eq!(all(&doc, "title")[0].text(), Some(evil));
        assert!(
            r.alt.contains(&evil.replace('\n', " ")) || r.alt.contains("titled"),
            "{}",
            r.alt
        );
    }
}

#[test]
fn markup_in_category_values_series_names_and_formats_is_inert_too() {
    let evil = HOSTILE[2];
    for chart in [
        "line",
        "scatter",
        "pie",
        "heatmap",
        "box",
        "histogram",
        "combo",
    ] {
        let data = json!({"columns": ["k", "s", "v", "w"], "rows": [[evil, evil, 1, 2], ["b", "t", 3, 4], ["c", evil, 5, 6]]});
        let extra = match chart {
            "scatter" => json!({"x": "v", "y": "w", "series": "s"}),
            "combo" => json!({"y": ["v"], "line": ["w"]}),
            "heatmap" => json!({"x": "k", "y": "s", "value": "v"}),
            "box" => json!({"x": "k", "y": "v"}),
            "histogram" => json!({"x": "v", "series": "s"}),
            "line" => json!({"x": "v", "y": ["w"], "series": "s"}),
            _ => json!({"x": "k", "y": ["v"]}),
        };
        let mut req = json!({"chart": chart, "data": data, "format": format!("{{{evil}}}{{,.0f}}"), "legend": "right"});
        let _ = &mut req;
        let mut m = req.as_object().unwrap().clone();
        m.remove("format");
        m.extend(extra.as_object().unwrap().clone());
        let r = ok(&Value::Object(m).to_string());
        let doc = parse(&r.svg);
        assert_only_drawing_elements(&doc);
        assert!(all(&doc, "script").is_empty(), "{chart}");
    }
}

#[test]
fn a_number_format_cannot_inject_markup_through_its_literal_text() {
    let r = ok(
        &json!({"chart": "bar", "format": "<b onclick=x>{,.0f}</b>\"", "data": "k,v\na,1\nb,2\n"})
            .to_string(),
    );
    let doc = parse(&r.svg);
    assert_only_drawing_elements(&doc);
    assert!(
        texts(&doc).iter().any(|t| t == "<b onclick=x>2</b>\""),
        "{:?}",
        texts(&doc)
    );
}

#[test]
fn control_characters_become_spaces_or_vanish_and_the_svg_stays_valid_xml() {
    let nasty = "a\u{0}b\nc\td\u{1}e\u{7f}f\u{FFFE}g\u{FFFF}h\r\ni";
    let r = ok(&json!({"chart": "bar", "title": nasty, "data": {"columns": ["k", "v"], "rows": [[nasty, 1]]}}).to_string());
    let doc = parse(&r.svg);
    let t = texts(&doc);
    assert!(
        t.iter()
            .any(|x| x == "ab c de\u{7f}fgh i" || x == "ab c defgh i" || x.starts_with("ab c d")),
        "{t:?}"
    );
    assert!(!r.svg.contains('\u{0}') && !r.svg.contains('\u{1}'));
    assert!(
        r.svg.chars().all(|c| c == '\n'
            || c == '\t'
            || c == '\r'
            || !c.is_control()
            || c == '\u{7f}'),
        "no control character in the document"
    );
}

#[test]
fn a_lone_surrogate_in_the_request_is_a_request_error_not_a_crash() {
    let e = crate::run("{\"chart\":\"bar\",\"title\":\"\\ud800\",\"data\":\"a,b\\nx,1\"}")
        .unwrap_err()
        .0;
    assert!(!e.is_empty() && !e.contains('\n'), "{e}");
}

#[test]
fn enormous_strings_are_shortened_and_the_output_stays_small() {
    let huge = "x".repeat(200_000);
    let big_cell = "y".repeat(60_000);
    let r = ok(&json!({
        "chart": "bar",
        "title": huge,
        "subtitle": huge,
        "x_label": huge,
        "y_label": huge,
        "data": {"columns": ["k", &huge], "rows": [[&big_cell, 1], ["b", 2]]},
    })
    .to_string());
    assert!(r.svg.len() < 600_000, "{} bytes", r.svg.len());
    let doc = parse(&r.svg);
    for t in texts(&doc) {
        assert!(
            t.chars().count() < 400,
            "a drawn text of {} characters",
            t.chars().count()
        );
    }
}

#[test]
fn thousands_of_distinct_long_labels_stay_bounded() {
    let rows: Vec<Value> = (0..5000)
        .map(|i| json!([format!("{}{i}", "category ".repeat(40)), i]))
        .collect();
    let r = ok(&json!({"chart": "bar", "data": {"columns": ["k", "v"], "rows": rows}}).to_string());
    assert!(r.svg.len() < 3_000_000, "{} bytes", r.svg.len());
    assert_eq!(r.series[0].points, 100);
}

#[test]
fn greek_cyrillic_and_accented_text_has_glyphs_and_draws_without_warnings() {
    let r = ok(&json!({"chart": "bar", "title": "Ανάλυση πωλήσεων \u{2013} Продажи по регионам", "data": {"columns": ["région", "v"], "rows": [["Île-de-France", 1], ["Ñandú", 2], ["Σύνολο", 3], ["Итого", 4]]}}).to_string());
    assert!(
        !r.warnings.iter().any(|w| w.contains("no glyph")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn cjk_emoji_and_other_missing_glyphs_are_reported_with_the_characters() {
    let r = ok(&json!({"chart": "bar", "title": "売上 \u{1F4C8}", "data": {"columns": ["k", "v"], "rows": [["東京", 1], ["大阪 \u{1F600}", 2]]}}).to_string());
    let w = r
        .warnings
        .iter()
        .find(|w| w.contains("no glyph"))
        .expect("a missing glyph warning");
    for c in ["売", "上", "\u{1F4C8}", "東", "京", "大", "阪", "\u{1F600}"] {
        assert!(w.contains(c), "{c} is named in {w:?}");
    }
    assert!(w.contains("drawn as empty boxes"), "{w}");
    parse(&r.svg);
}

#[test]
fn more_than_eight_missing_characters_are_counted_not_listed() {
    let r = ok(
        &json!({"chart": "bar", "title": "一二三四五六七八九十", "data": "k,v\na,1\n"}).to_string(),
    );
    let w = r.warnings.iter().find(|w| w.contains("no glyph")).unwrap();
    assert!(
        w.starts_with("10 characters have no glyph") && w.contains("and 2 more"),
        "{w}"
    );
}

#[test]
fn text_the_font_cannot_draw_still_makes_a_picture_that_differs_from_no_text() {
    let png = |title: &str| {
        let req =
            json!({"chart": "bar", "title": title, "output": "png", "data": "k,v\na,1\nb,2\n"})
                .to_string();
        let v: Value = serde_json::from_str(&crate::run(&req).unwrap()).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(v["parts"][0]["data"].as_str().unwrap())
            .unwrap();
        raster::decode(&bytes).unwrap()
    };
    let (w, _, with) = png("漢字漢字");
    let (_, _, without) = png("");
    let band = 16 * w as usize * 3..40 * w as usize * 3;
    assert_ne!(
        with[band.clone()],
        without[band],
        "the title band differs: something was drawn for the missing glyphs"
    );
}

#[test]
fn right_to_left_and_mixed_direction_text_does_not_panic_and_keeps_its_characters() {
    for text in [
        "שלום עולם",
        "مرحبا بالعالم",
        "abc שלום 123 مرحبا",
        "\u{202E}reversed\u{202C}",
        "\u{200F}x\u{200E}",
        "a\u{0301}\u{0302}\u{0303}",
    ] {
        let r = ok(&json!({"chart": "bar", "title": text, "data": {"columns": ["k", "v"], "rows": [[text, 1], ["b", 2]]}}).to_string());
        let doc = parse(&r.svg);
        assert!(texts(&doc).iter().any(|t| t == text), "{text:?}");
        let png = r.png.as_ref().unwrap();
        assert!(png.bytes.len() > 100);
    }
}

#[test]
fn zero_width_joined_emoji_and_combining_sequences_do_not_panic() {
    for text in [
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "\u{1F1EF}\u{1F1F5}",
        "e\u{301}\u{301}\u{301}\u{301}",
        "\u{0}\u{200B}\u{2060}\u{FEFF}",
    ] {
        let r = ok(&json!({"chart": "pie", "data": {"columns": ["k", "v"], "rows": [[text, 1], ["b", 2]]}}).to_string());
        parse(&r.svg);
    }
}

#[test]
fn duplicate_empty_and_numeric_looking_names_are_kept_apart() {
    let r = ok(r#"{"chart":"line","data":"x,a,a,,5\n1,2,3,4,5\n2,3,4,5,6\n"}"#);
    let names: Vec<&str> = r.series.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["a", "a_2", "column 4", "5"]);
}
