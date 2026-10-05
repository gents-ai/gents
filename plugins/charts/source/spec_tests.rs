//! Tests of request validation and defaults.

use super::*;

fn req(json: &str) -> Res<Spec> {
    let r: Request =
        serde_json::from_str(json).map_err(|e| crate::err::ChartError(e.to_string()))?;
    r.resolve()
}

#[test]
fn a_minimal_request_gets_the_defaults() {
    let s = req(r#"{"chart":"line"}"#).unwrap();
    assert_eq!(
        (s.kind, s.width, s.height, s.scale),
        (Kind::Line, 800, 480, 1.0)
    );
    assert_eq!(
        (s.agg, s.sort, s.legend, s.output),
        (Agg::Sum, Sort::None, Legend::Auto, Output::Both)
    );
    assert!(!s.theme.dark && !s.stacked && !s.horizontal && !s.y_log);
    assert_eq!(s.bins, Bins::Auto);
}

#[test]
fn pies_default_to_a_squarer_canvas() {
    assert_eq!(req(r#"{"chart":"pie"}"#).unwrap().height, 520);
}

#[test]
fn chart_type_aliases_set_the_layout() {
    let s = req(r#"{"chart":"stacked_bar"}"#).unwrap();
    assert!(s.kind == Kind::Bar && s.stacked && !s.horizontal);
    let s = req(r#"{"chart":"horizontal_bar"}"#).unwrap();
    assert!(s.kind == Kind::Bar && s.horizontal);
    assert_eq!(req(r#"{"type":"column"}"#).unwrap().kind, Kind::Bar);
    assert_eq!(req(r#"{"chart":"boxplot"}"#).unwrap().kind, Kind::Box);
    let s = req(r#"{"chart":"bar","stack":"stacked","horizontal":true}"#).unwrap();
    assert!(s.stacked && s.horizontal);
}

#[test]
fn every_chart_type_name_resolves_and_round_trips_its_name() {
    for name in [
        "line",
        "area",
        "stacked_area",
        "bar",
        "scatter",
        "bubble",
        "histogram",
        "box",
        "pie",
        "donut",
        "heatmap",
        "combo",
    ] {
        assert_eq!(
            req(&format!(r#"{{"chart":"{name}"}}"#))
                .unwrap()
                .kind
                .name(),
            name
        );
    }
}

#[test]
fn missing_and_unknown_chart_types_are_refused() {
    assert!(req("{}").unwrap_err().0.starts_with("chart is required"));
    let e = req(r#"{"chart":"radar"}"#).unwrap_err().0;
    assert!(
        e.contains("\"radar\" is not known") && e.contains("combo"),
        "{e}"
    );
}

#[test]
fn unknown_fields_are_rejected() {
    let e = req(r#"{"chart":"line","titel":"x"}"#).unwrap_err().0;
    assert!(e.contains("titel"), "{e}");
}

#[test]
fn size_limits_are_enforced_with_the_numbers() {
    for (json, needle) in [
        (
            r#"{"chart":"line","width":100}"#,
            "width must be between 200 and 4096",
        ),
        (r#"{"chart":"line","width":5000}"#, "width must be between"),
        (
            r#"{"chart":"line","height":100}"#,
            "height must be between 150 and 4096",
        ),
        (
            r#"{"chart":"line","scale":0.1}"#,
            "scale must be between 0.5 and 4",
        ),
        (r#"{"chart":"line","scale":9}"#, "scale must be between"),
        (
            r#"{"chart":"line","width":4096,"height":4096}"#,
            "over the limit of 16000000",
        ),
        (
            r#"{"chart":"line","width":3000,"height":2000,"scale":2}"#,
            "pixels",
        ),
    ] {
        let e = req(json).unwrap_err().0;
        assert!(e.contains(needle), "{json}: {e}");
    }
    assert!(req(r#"{"chart":"line","width":4000,"height":4000}"#).is_ok());
    assert!(req(r#"{"chart":"line","width":200,"height":150,"scale":4}"#).is_ok());
}

#[test]
fn choice_fields_name_the_valid_values() {
    for (json, field) in [
        (r#"{"chart":"line","agg":"mode"}"#, "agg"),
        (r#"{"chart":"line","sort":"up"}"#, "sort"),
        (r#"{"chart":"line","legend":"inside"}"#, "legend"),
        (r#"{"chart":"line","output":"pdf"}"#, "output"),
        (r#"{"chart":"line","stack":"piled"}"#, "stack"),
        (r#"{"chart":"line","theme":"sepia"}"#, "theme"),
        (r#"{"chart":"line","line_axis":"top"}"#, "line_axis"),
        (r#"{"chart":"line","y_scale":"time"}"#, "y_scale"),
        (r#"{"chart":"line","x_scale":"sqrt"}"#, "x_scale"),
    ] {
        let e = req(json).unwrap_err().0;
        assert!(e.contains(field), "{json}: {e}");
    }
}

#[test]
fn bounds_must_be_ordered_and_finite() {
    assert!(
        req(r#"{"chart":"line","y_min":5,"y_max":5}"#)
            .unwrap_err()
            .0
            .contains("y_min must be below y_max")
    );
    assert!(
        req(r#"{"chart":"line","x_min":9,"x_max":1}"#)
            .unwrap_err()
            .0
            .contains("x_min must be below x_max")
    );
    assert!(req(r#"{"chart":"line","y_min":1e999}"#).is_err());
    assert!(req(r#"{"chart":"line","y_min":0,"y_max":10}"#).is_ok());
}

#[test]
fn bins_accept_auto_or_a_count_in_range() {
    assert_eq!(
        req(r#"{"chart":"histogram","bins":"auto"}"#).unwrap().bins,
        Bins::Auto
    );
    assert_eq!(
        req(r#"{"chart":"histogram","bins":12}"#).unwrap().bins,
        Bins::Count(12)
    );
    assert_eq!(
        req(r#"{"chart":"histogram","bins":"12"}"#).unwrap().bins,
        Bins::Count(12),
        "a graph record carries bins as text"
    );
    for bad in [r#""many""#, r#""0""#, r#""201""#, "0", "201", "-1", "2.5"] {
        assert!(
            req(&format!(r#"{{"chart":"histogram","bins":{bad}}}"#)).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn y_may_be_a_name_or_a_list_and_blank_names_drop_out() {
    assert_eq!(req(r#"{"chart":"line","y":"a"}"#).unwrap().y, ["a"]);
    assert_eq!(
        req(r#"{"chart":"line","y":["a","","b"]}"#).unwrap().y,
        ["a", "b"]
    );
    let many: Vec<String> = (0..25).map(|i| format!("c{i}")).collect();
    let e = req(&format!(
        r#"{{"chart":"line","y":{}}}"#,
        serde_json::to_string(&many).unwrap()
    ))
    .unwrap_err()
    .0;
    assert!(e.contains("at most 24"), "{e}");
}

#[test]
fn options_that_do_not_apply_are_noted_not_silently_dropped() {
    let s = req(r#"{"chart":"line","stack":"stacked","horizontal":true,"line":["a"]}"#).unwrap();
    assert_eq!(s.notes.len(), 3, "{:?}", s.notes);
    assert!(!s.stacked && !s.horizontal && s.line.is_empty());
}

#[test]
fn text_fields_are_cleaned_and_empty_ones_dropped() {
    let s = req("{\"chart\":\"line\",\"title\":\"  Sales\\nQ1 \",\"subtitle\":\"   \"}").unwrap();
    assert_eq!(s.title.as_deref(), Some("Sales Q1"));
    assert_eq!(s.subtitle, None);
}

#[test]
fn formats_are_parsed_once_and_bad_ones_fail() {
    let s = req(r#"{"chart":"line","format":",.0f"}"#).unwrap();
    assert!(s.y_format.is_some() && s.format.is_some());
    let s = req(r#"{"chart":"line","format":",.0f","y_format":".1%"}"#).unwrap();
    assert_eq!(s.y_format.unwrap().kind, crate::format::Kind::Percent);
    assert!(
        req(r#"{"chart":"line","y_format":"zz"}"#)
            .unwrap_err()
            .0
            .contains("not understood")
    );
    assert!(
        req(r#"{"chart":"line","x_format":"%q"}"#)
            .unwrap_err()
            .0
            .contains("%q")
    );
    assert!(req(r#"{"chart":"line","x_format":"%Y-%m"}"#).is_ok());
    assert!(req(r#"{"chart":"line","x_format":".0%"}"#).is_ok());
}

#[test]
fn colors_are_validated() {
    let s = req(r##"{"chart":"line","colors":["#F00","#00ff00"]}"##).unwrap();
    assert_eq!(s.colors.unwrap(), ["#ff0000", "#00ff00"]);
    assert!(req(r##"{"chart":"line","colors":["red"]}"##).is_err());
}

#[test]
fn save_names_must_be_relative_with_the_right_extension() {
    let s = req(r#"{"chart":"line","save":"out/chart"}"#)
        .unwrap()
        .save
        .unwrap();
    assert_eq!(
        (s.svg.as_deref(), s.png.as_deref()),
        (Some("out/chart.svg"), Some("out/chart.png"))
    );
    let s = req(r#"{"chart":"line","save":{"png":"a.PNG"}}"#)
        .unwrap()
        .save
        .unwrap();
    assert_eq!((s.svg, s.png.as_deref()), (None, Some("a.PNG")));
    for bad in [
        "../a.svg",
        "/a.svg",
        "a/../b.svg",
        "a\\b.svg",
        "a.txt",
        ".svg",
        "a//b.svg",
        "./a.svg",
        "C:/a.svg",
        "a/.svg",
    ] {
        let e = save_name(bad, "svg").unwrap_err().0;
        assert!(e.contains("save file"), "{bad}: {e}");
    }
    assert!(req(r#"{"chart":"line","save":{}}"#).is_err());
    assert!(req(r#"{"chart":"line","save":""}"#).is_err());
    assert!(req(r#"{"chart":"line","save":"../x"}"#).is_err());
    assert!(save_name(&format!("{}.svg", "a".repeat(300)), "svg").is_err());
}

#[test]
fn needed_columns_are_listed_only_when_the_request_is_complete() {
    let s = req(r#"{"chart":"line","x":"m","y":["a","b"],"series":"r"}"#).unwrap();
    assert_eq!(s.needed_columns().unwrap(), ["a", "b", "m", "r"]);
    assert_eq!(
        req(r#"{"chart":"line","x":"m"}"#).unwrap().needed_columns(),
        None
    );
    assert_eq!(
        req(r#"{"chart":"histogram","x":"v"}"#)
            .unwrap()
            .needed_columns()
            .unwrap(),
        ["v"]
    );
    assert_eq!(
        req(r#"{"chart":"box","y":"v","x":"g"}"#)
            .unwrap()
            .needed_columns()
            .unwrap(),
        ["g", "v"]
    );
    assert_eq!(
        req(r#"{"chart":"heatmap","x":"a","y":"b"}"#)
            .unwrap()
            .needed_columns(),
        None
    );
    assert!(
        req(r#"{"chart":"heatmap","x":"a","y":"b","value":"c"}"#)
            .unwrap()
            .needed_columns()
            .is_some()
    );
    assert_eq!(req(r#"{"chart":"pie"}"#).unwrap().needed_columns(), None);
}

#[test]
fn inline_data_is_borrowed_not_copied() {
    let json = r#"{"chart":"line","data":{"columns":["a"],"rows":[[1]]}}"#;
    let r: Request = serde_json::from_str(json).unwrap();
    let raw = r.data.unwrap().get();
    assert_eq!(raw, r#"{"columns":["a"],"rows":[[1]]}"#);
    let start = json.as_ptr() as usize;
    let at = raw.as_ptr() as usize;
    assert!(
        at >= start && at < start + json.len(),
        "the raw value points into the request text"
    );
}
