//! Whole requests: data sources, the result shape, saving files and the
//! sentences for everything that can go wrong.

use base64::Engine as _;
use serde_json::{Value, json};

use crate::raster;
use crate::testutil::TempDir;

fn run(json: &str) -> Value {
    serde_json::from_str(&crate::run(json).unwrap_or_else(|e| panic!("{json} failed: {e}")))
        .unwrap()
}

fn fails(json: &str) -> String {
    match crate::run(json) {
        Ok(_) => panic!("{json} should have failed"),
        Err(e) => e.0,
    }
}

fn one_line(sentence: &str) {
    assert!(
        !sentence.is_empty() && !sentence.contains('\n') && sentence.len() < 400,
        "{sentence:?}"
    );
    assert!(
        !sentence.contains('\u{2014}') && !sentence.contains('\u{2013}'),
        "no dashes: {sentence:?}"
    );
}

const ROWS: &str = r#"{"columns":["k","v"],"rows":[["a",1],["b",2]]}"#;

#[test]
fn the_result_has_the_documented_shape() {
    let v = run(&format!(r#"{{"chart":"bar","title":"T","data":{ROWS}}}"#));
    let resp = v["response"].as_object().unwrap();
    let keys: Vec<&str> = resp.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "alt", "chart", "height", "png", "series", "svg", "warnings", "width"
        ]
    );
    assert_eq!(resp["chart"], "bar");
    assert_eq!(
        (resp["width"].as_u64(), resp["height"].as_u64()),
        (Some(800), Some(480))
    );
    assert_eq!(
        resp["png"]["bytes"].as_u64(),
        Some(base64_len(&v["parts"][0]["data"]))
    );
    assert_eq!(resp["series"][0]["name"], "v");
    let series_keys: Vec<&str> = resp["series"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        series_keys,
        [
            "axis", "color", "gaps", "mark", "max", "min", "name", "points", "sum"
        ]
    );
    assert_eq!(v["parts"].as_array().unwrap().len(), 1);
}

fn base64_len(data: &Value) -> u64 {
    base64::engine::general_purpose::STANDARD
        .decode(data.as_str().unwrap())
        .unwrap()
        .len() as u64
}

#[test]
fn the_image_part_is_a_png_of_the_stated_size() {
    let v = run(&format!(
        r#"{{"chart":"line","width":500,"height":300,"scale":2,"data":{ROWS}}}"#
    ));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(v["parts"][0]["data"].as_str().unwrap())
        .unwrap();
    let (w, h, rgb) = raster::decode(&bytes).unwrap();
    assert_eq!((w, h), (1000, 600));
    assert_eq!(rgb.len(), 1000 * 600 * 3);
    assert_eq!(
        (
            v["response"]["png"]["width"].as_u64(),
            v["response"]["png"]["height"].as_u64()
        ),
        (Some(1000), Some(600))
    );
    assert_eq!(
        (
            v["response"]["width"].as_u64(),
            v["response"]["height"].as_u64()
        ),
        (Some(500), Some(300)),
        "the logical size does not change with the scale"
    );
    let svg = v["response"]["svg"].as_str().unwrap();
    assert!(svg.contains("width=\"500\" height=\"300\" viewBox=\"0 0 500 300\""));
}

#[test]
fn output_chooses_what_comes_back() {
    let svg_only = run(&format!(
        r#"{{"chart":"bar","output":"svg","data":{ROWS}}}"#
    ));
    assert!(
        svg_only.get("parts").is_none()
            && svg_only["response"]["svg"].is_string()
            && svg_only["response"].get("png").is_none()
    );
    let png_only = run(&format!(
        r#"{{"chart":"bar","output":"png","data":{ROWS}}}"#
    ));
    assert!(
        png_only["parts"].is_array()
            && png_only["response"].get("svg").is_none()
            && png_only["response"]["alt"].is_string()
    );
}

#[test]
fn inline_data_may_be_columns_and_rows_objects_or_csv_text() {
    for data in [
        ROWS.to_owned(),
        r#"[{"k":"a","v":1},{"k":"b","v":2}]"#.to_owned(),
        json!("k,v\na,1\nb,2\n").to_string(),
        json!(ROWS).to_string(),
        r#"{"response":{"columns":["k","v"],"rows":[["a",1],["b",2]]},"parts":[]}"#.to_owned(),
        r#"{"columns":[{"name":"k"},{"name":"v","type":"int"}],"rows":[["a",1],["b",2]]}"#
            .to_owned(),
    ] {
        let v = run(&format!(r#"{{"chart":"bar","data":{data}}}"#));
        assert_eq!(v["response"]["series"][0]["points"], 2, "{data}");
    }
}

#[test]
fn a_csv_file_is_read_by_path_and_by_folder_and_file() {
    let d = TempDir::new();
    let file = d.write("sub/sales.csv", b"k,v\na,1\nb,2\nc,3\n");
    let by_file = run(&format!(
        r#"{{"chart":"bar","path":{}}}"#,
        json!(file.to_str().unwrap())
    ));
    let by_folder = run(&format!(
        r#"{{"chart":"bar","path":{},"file":"sub/sales.csv"}}"#,
        json!(d.path().to_str().unwrap())
    ));
    assert_eq!(by_file["response"]["svg"], by_folder["response"]["svg"]);
    assert_eq!(by_file["response"]["series"][0]["points"], 3);
}

#[test]
fn json_and_json_lines_files_work_and_the_content_decides_unknown_extensions() {
    let d = TempDir::new();
    let p = |n: &str, body: &str| json!(d.write(n, body.as_bytes()).to_str().unwrap()).to_string();
    for (name, body) in [
        ("a.json", r#"[{"k":"a","v":1},{"k":"b","v":2}]"#),
        ("b.json", ROWS),
        ("c.jsonl", "{\"k\":\"a\",\"v\":1}\n{\"k\":\"b\",\"v\":2}\n"),
        ("d.ndjson", "{\"k\":\"a\",\"v\":1}\n\n{\"k\":\"b\",\"v\":2}"),
        ("e.dat", ROWS),
        ("f.txt", "k,v\na,1\nb,2\n"),
        ("g.png", "k;v\na;1\nb;2\n"),
        ("h", "k\tv\na\t1\nb\t2\n"),
    ] {
        let v = run(&format!(r#"{{"chart":"bar","path":{}}}"#, p(name, body)));
        assert_eq!(v["response"]["series"][0]["points"], 2, "{name}");
    }
}

#[test]
fn a_utf8_bom_and_crlf_line_endings_are_handled() {
    let d = TempDir::new();
    let p = d.write("bom.csv", b"\xef\xbb\xbfk,v\r\na,1\r\nb,2\r\n");
    let v = run(&format!(
        r#"{{"chart":"bar","path":{}}}"#,
        json!(p.to_str().unwrap())
    ));
    assert_eq!(v["response"]["series"][0]["points"], 2);
}

#[test]
fn the_data_source_rules_have_sentences() {
    let d = TempDir::new();
    let f = d.write("a.csv", b"k,v\na,1\n");
    let dir = json!(d.path().to_str().unwrap());
    let file = json!(f.to_str().unwrap());
    for (req, needle) in [
        (r#"{"chart":"bar"}"#.to_owned(), "there is no data"),
        (
            format!(r#"{{"chart":"bar","data":{ROWS},"file":"a.csv"}}"#),
            "either data or a file",
        ),
        (
            format!(r#"{{"chart":"bar","path":{dir}}}"#),
            "path is a folder",
        ),
        (
            format!(r#"{{"chart":"bar","path":{file},"file":"a.csv"}}"#),
            "already a file",
        ),
        (
            format!(r#"{{"chart":"bar","path":{dir},"file":"missing.csv"}}"#),
            "missing.csv does not exist",
        ),
        (
            r#"{"chart":"bar","path":"/no/such/place"}"#.to_owned(),
            "does not exist",
        ),
        (r#"{"chart":"bar","data":""}"#.to_owned(), "data is empty"),
        (
            r#"{"chart":"bar","data":"   "}"#.to_owned(),
            "data is empty",
        ),
        (
            r#"{"chart":"bar","data":{"columns":["a"],"rows":[]}}"#.to_owned(),
            "no rows",
        ),
        (r#"{"chart":"bar","data":"k,v\n"}"#.to_owned(), "no rows"),
        (
            r#"{"chart":"bar","data":42}"#.to_owned(),
            "data must be columns and rows",
        ),
        (
            r#"{"chart":"bar","data":null}"#.to_owned(),
            "there is no data",
        ),
        (
            r#"{"chart":"bar","data":true}"#.to_owned(),
            "data must be columns and rows",
        ),
    ] {
        let e = fails(&req);
        assert!(e.contains(needle), "{req}: {e}");
        one_line(&e);
    }
}

#[test]
fn paths_that_leave_the_folder_are_refused_before_anything_is_read() {
    let d = TempDir::new();
    let outer = d.write("secret.csv", b"k,v\nleak,1\n");
    let inner = d.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    let dir = json!(inner.to_str().unwrap());
    for bad in [
        "../secret.csv",
        "sub/../../secret.csv",
        outer.to_str().unwrap(),
        "/etc/passwd",
        "..\\secret.csv",
        "C:/secret.csv",
    ] {
        let e = fails(&format!(
            r#"{{"chart":"bar","path":{dir},"file":{}}}"#,
            json!(bad)
        ));
        assert!(
            e.contains("leaves the folder") || e.contains("not a path inside the folder"),
            "{bad}: {e}"
        );
        one_line(&e);
    }
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_inside_the_folder_is_not_followed_out_of_it() {
    let d = TempDir::new();
    let outside = TempDir::new();
    outside.write("secret.csv", b"k,v\nleak,1\n");
    std::os::unix::fs::symlink(outside.path().join("secret.csv"), d.path().join("link.csv"))
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("dir")).unwrap();
    let dir = json!(d.path().to_str().unwrap());
    for name in ["link.csv", "dir/secret.csv"] {
        let e = fails(&format!(
            r#"{{"chart":"bar","path":{dir},"file":"{name}"}}"#
        ));
        assert!(e.contains("symbolic link"), "{name}: {e}");
    }
}

#[test]
fn unreadable_files_get_one_sentence_each() {
    let d = TempDir::new();
    let p = |name: &str, bytes: &[u8]| json!(d.write(name, bytes).to_str().unwrap()).to_string();
    for (name, bytes, needle) in [
        ("empty.csv", &b""[..], "is empty"),
        (
            "binary.csv",
            &[0x89, b'P', b'N', b'G', 0, 0, 0, 1][..],
            "looks binary",
        ),
        (
            "truncated.json",
            &br#"[{"k":"a","v":1},{"k":"#[..],
            "truncated",
        ),
        ("garbage.json", &b"this is not json"[..], "JSON"),
        ("headeronly.csv", &b"k,v\n"[..], "no rows"),
        (
            "huge.csv",
            &[b"k\n".to_vec(), vec![b'x'; 70_000]].concat()[..],
            "longer than 65536 bytes",
        ),
    ] {
        let e = fails(&format!(r#"{{"chart":"bar","path":{}}}"#, p(name, bytes)));
        assert!(e.contains(needle), "{name}: {e}");
        one_line(&e);
    }
}

#[test]
fn ragged_rows_nested_values_and_bad_utf8_are_reported() {
    let v = run(r#"{"chart":"bar","data":"k,v\na,1\nb,2,3\nc\n"}"#);
    let w = v["response"]["warnings"].to_string();
    assert!(
        w.contains("2 rows have a different number of fields"),
        "{w}"
    );
    let v = run(
        r#"{"chart":"bar","data":[{"k":"a","v":1,"extra":[1,2]},{"k":"b","v":2,"extra":{"x":1}}]}"#,
    );
    assert!(
        v["response"]["warnings"]
            .to_string()
            .contains("2 values were lists or objects"),
        "{v}"
    );
    let d = TempDir::new();
    let p = d.write("bad.csv", b"k,v\na\xff,1\nb,2\n");
    let v = run(&format!(
        r#"{{"chart":"bar","path":{}}}"#,
        json!(p.to_str().unwrap())
    ));
    assert!(
        v["response"]["warnings"]
            .to_string()
            .contains("not valid UTF-8")
    );
}

#[test]
fn only_the_named_columns_are_kept_so_a_wide_file_costs_little() {
    let mut csv = String::new();
    csv.push_str(
        &(0..600)
            .map(|i| format!("c{i}"))
            .collect::<Vec<_>>()
            .join(","),
    );
    csv.push('\n');
    for r in 0..3 {
        csv.push_str(
            &(0..600)
                .map(|i| (i + r).to_string())
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push('\n');
    }
    let named = run(&format!(
        r#"{{"chart":"line","x":"c0","y":["c599"],"data":{}}}"#,
        json!(csv)
    ));
    assert_eq!(
        named["response"]["warnings"],
        json!([]),
        "naming the columns avoids the column limit"
    );
    let all = run(&format!(r#"{{"chart":"line","data":{}}}"#, json!(csv)));
    assert!(
        all["response"]["warnings"]
            .to_string()
            .contains("more than 512 columns"),
        "{all}"
    );
    assert_eq!(
        all["response"]["series"].as_array().unwrap().len(),
        24,
        "capped at 24 series"
    );
    assert!(
        all["response"]["warnings"]
            .to_string()
            .contains("only the first 24 are drawn")
    );
}

#[test]
fn a_file_with_more_rows_than_the_limit_is_read_to_the_limit_and_says_so() {
    let d = TempDir::new();
    let mut body = String::from("x,y\n");
    body.reserve(2_100_000 * 12);
    for i in 0..2_100_000u32 {
        body.push_str(&format!("{i},{}\n", i % 100));
    }
    let p = d.write("big.csv", body.as_bytes());
    let v = run(&format!(
        r#"{{"chart":"line","x":"x","y":["y"],"path":{}}}"#,
        json!(p.to_str().unwrap())
    ));
    let w = v["response"]["warnings"].to_string();
    assert!(
        w.contains("only the first 2000000 rows were read")
            && w.contains("the limit is 2000000 rows"),
        "{w}"
    );
    let s = &v["response"]["series"][0];
    assert_eq!(s["points"], 2_000_000);
    assert!(s["drawn"].as_u64().unwrap() <= 1502, "{s}");
}

#[test]
fn bad_requests_get_one_sentence() {
    for (raw, needle) in [
        ("", "ends early"),
        ("{", "ends early"),
        ("not json", "not valid"),
        ("[]", "not valid"),
        (
            r#"{"chart":"bar","titel":"x"}"#,
            "field \"titel\" is not known",
        ),
        (r#"{"chart":"bar","width":"wide"}"#, "not valid"),
        (r#"{"chart":"radar"}"#, "chart type \"radar\" is not known"),
        (r#"{"data":"a"}"#, "chart is required"),
        (
            r#"{"chart":"bar","width":10,"data":"a,b\n1,2"}"#,
            "width must be between",
        ),
        (
            r#"{"chart":"bar","scale":9,"data":"a,b\n1,2"}"#,
            "scale must be between",
        ),
        (
            r#"{"chart":"bar","x":"nope","data":"a,b\n1,2"}"#,
            "column \"nope\" is not in the data; the columns are a, b",
        ),
    ] {
        let e = fails(raw);
        assert!(e.contains(needle), "{raw}: {e}");
        one_line(&e);
    }
}

#[test]
fn the_pixel_cap_applies_to_the_scaled_size() {
    let e = fails(r#"{"chart":"bar","width":4096,"height":4096,"data":"a,b\nx,1\n"}"#);
    assert!(
        e.contains("16000000") && e.contains("lower width, height or scale"),
        "{e}"
    );
    one_line(&e);
    let v = run(r#"{"chart":"bar","width":2000,"height":2000,"data":"a,b\nx,1\n"}"#);
    assert_eq!(v["response"]["png"]["width"], 2000);
}

#[test]
fn saving_writes_the_svg_and_png_that_the_result_describes() {
    let d = TempDir::new();
    d.write("data.csv", b"k,v\na,1\nb,2\n");
    let req = format!(
        r#"{{"chart":"bar","path":{},"file":"data.csv","save":"out/sales"}}"#,
        json!(d.path().to_str().unwrap())
    );
    let v = run(&req);
    let files = v["response"]["files"].as_array().unwrap();
    assert_eq!(
        files
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["out/sales.svg", "out/sales.png"]
    );
    let svg = std::fs::read_to_string(d.path().join("out/sales.svg")).unwrap();
    assert_eq!(svg, v["response"]["svg"].as_str().unwrap());
    let png = std::fs::read(d.path().join("out/sales.png")).unwrap();
    assert_eq!(
        png,
        base64::engine::general_purpose::STANDARD
            .decode(v["parts"][0]["data"].as_str().unwrap())
            .unwrap()
    );
    assert_eq!(files[0]["bytes"].as_u64(), Some(svg.len() as u64));
    assert_eq!(files[1]["bytes"].as_u64(), Some(png.len() as u64));
}

#[test]
fn saving_only_one_format_writes_only_that_file() {
    let d = TempDir::new();
    let dir = json!(d.path().to_str().unwrap());
    run(&format!(
        r#"{{"chart":"bar","data":{ROWS},"path":{dir},"save":{{"png":"only.png"}}}}"#
    ));
    assert!(d.path().join("only.png").is_file() && !d.path().join("only.svg").exists());
    run(&format!(
        r#"{{"chart":"bar","output":"svg","data":{ROWS},"path":{dir},"save":{{"svg":"s.svg","png":"p.png"}}}}"#
    ));
    assert!(
        d.path().join("s.svg").is_file() && d.path().join("p.png").is_file(),
        "saving renders the PNG even when only the SVG is returned"
    );
}

#[test]
fn saving_refuses_bad_destinations_and_writes_nothing() {
    let d = TempDir::new();
    let f = d.write("data.csv", b"k,v\na,1\n");
    let dir = json!(d.path().to_str().unwrap());
    let file = json!(f.to_str().unwrap());
    for (req, needle) in [
        (
            format!(r#"{{"chart":"bar","data":{ROWS},"save":"x"}}"#),
            "give the folder in path",
        ),
        (
            format!(r#"{{"chart":"bar","path":{file},"save":"x"}}"#),
            "give the folder in path",
        ),
        (
            format!(r#"{{"chart":"bar","data":{ROWS},"path":{dir},"save":"../x"}}"#),
            "stay inside the folder",
        ),
        (
            format!(r#"{{"chart":"bar","data":{ROWS},"path":{dir},"save":{{"svg":"x.png"}}}}"#),
            "ending in .svg",
        ),
        (
            format!(
                r#"{{"chart":"bar","data":{ROWS},"path":{dir},"save":{{"svg":"/tmp/x.svg"}}}}"#
            ),
            "relative path",
        ),
        (
            format!(r#"{{"chart":"bar","data":{ROWS},"path":{dir},"save":{{}}}}"#),
            "svg or a png file name",
        ),
    ] {
        let e = fails(&req);
        assert!(e.contains(needle), "{req}: {e}");
        one_line(&e);
    }
    let names: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "only the data file is there: {names:?}");
}

#[test]
fn rendering_twice_gives_byte_identical_output() {
    for chart in [
        "line",
        "area",
        "bar",
        "scatter",
        "histogram",
        "box",
        "pie",
        "donut",
    ] {
        let req =
            format!(r#"{{"chart":"{chart}","title":"T","data":"k,v\n1,5\n2,9\n3,4\n4,12\n"}}"#);
        assert_eq!(
            crate::run(&req).unwrap(),
            crate::run(&req).unwrap(),
            "{chart}"
        );
    }
}

#[test]
fn every_chart_type_answers_the_same_data_without_naming_columns() {
    let data = r#"k,a,b
x,1,3
y,2,4
z,3,5
"#;
    for chart in [
        "line",
        "area",
        "stacked_area",
        "bar",
        "stacked_bar",
        "horizontal_bar",
        "histogram",
        "box",
        "pie",
        "donut",
    ] {
        let req = format!(r#"{{"chart":"{chart}","data":{}}}"#, json!(data));
        let v = run(&req);
        assert!(
            v["response"]["svg"].as_str().unwrap().starts_with("<svg"),
            "{chart}"
        );
    }
}
