//! Mutation fuzzing and hostile inputs. Every fixture is corrupted many ways
//! with a fixed seed; the only acceptable outcomes are a valid result or one
//! plain sentence, within a deadline, and never a panic or a file outside
//! the folder.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::raster;
use crate::testkit::*;
use crate::testutil::TempDir;

/// xorshift64*: small, fixed, the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const TOKENS: [&[u8]; 14] = [
    b"1e999",
    b"-1e999",
    b"NaN",
    b"\0",
    b"\xff\xfe",
    b"\"",
    b",",
    b"\n",
    b"[",
    b"}",
    b"99999999999999999999999999999999",
    b"-0",
    b"\xe2\x80\xae",
    b"\xf0\x9f\x98",
];

/// One to four corruptions of `base`.
fn mutate(base: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = base.to_vec();
    for _ in 0..=rng.below(4) {
        if b.is_empty() {
            b.push(b'a');
        }
        match rng.below(8) {
            0 => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            1 => {
                let i = rng.below(b.len());
                b[i] = rng.next() as u8;
            }
            2 => {
                let i = rng.below(b.len());
                let j = (i + rng.below(40)).min(b.len());
                b.drain(i..j);
            }
            3 => {
                let i = rng.below(b.len() + 1);
                let t = TOKENS[rng.below(TOKENS.len())];
                b.splice(i..i, t.iter().copied());
            }
            4 => b.truncate(rng.below(b.len() + 1)),
            5 => {
                let i = rng.below(b.len());
                let j = (i + rng.below(60)).min(b.len());
                let chunk = b[i..j].to_vec();
                let at = rng.below(b.len() + 1);
                b.splice(at..at, chunk);
            }
            6 => {
                let i = rng.below(b.len());
                let j = rng.below(b.len());
                b.swap(i, j);
            }
            _ => {
                let i = rng.below(b.len() + 1);
                let n = rng.below(20);
                let junk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                b.splice(i..i, junk);
            }
        }
    }
    b
}

fn sentence_ok(e: &str) {
    assert!(
        !e.is_empty() && !e.contains('\n') && e.chars().count() <= 700,
        "not one sentence: {e:?}"
    );
}

/// Runs a request; a failure must be one sentence, a success a valid result.
/// Returns true when the request succeeded.
fn check(req: &str, deep: bool) -> bool {
    let started = Instant::now();
    let ok = match crate::run(req) {
        Err(e) => {
            sentence_ok(&e.0);
            false
        }
        Ok(out) => {
            let v: Value = serde_json::from_str(&out).expect("the result is JSON");
            if let Some(svg) = v["response"]["svg"].as_str() {
                let _ = parse(svg);
            }
            if deep && let Some(data) = v["parts"][0]["data"].as_str() {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .expect("base64");
                raster::decode(&bytes).expect("a valid PNG");
            }
            true
        }
    };
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "took {:?}",
        started.elapsed()
    );
    ok
}

const CSV: &[u8] = b"day,region,sales,margin\n2024-01-01,North,10.5,0.21\n2024-01-02,\"South, East\",12,0.25\n2024-01-03,West,,0.3\n2024-01-04,\"N\"\"orth\",9,\n2024-01-05,\xc3\xa9t\xc3\xa9,15,0.4\n";
const JSON_ROWS: &[u8] = br#"{"columns":["day","region","sales","margin"],"rows":[["2024-01-01","North",10.5,0.21],["2024-01-02","South",12,0.25],["2024-01-03","West",null,0.3],["2024-01-04","East",9,null]]}"#;
const JSON_OBJECTS: &[u8] = br#"[{"day":"2024-01-01","region":"North","sales":10.5,"margin":0.21},{"day":"2024-01-02","region":"South","sales":12,"margin":0.25},{"day":"2024-01-03","region":"West","sales":"n/a","margin":0.3}]"#;
const NDJSON: &[u8] = b"{\"day\":\"2024-01-01\",\"region\":\"North\",\"sales\":10.5}\n{\"day\":\"2024-01-02\",\"region\":\"South\",\"sales\":12}\n{\"day\":\"2024-01-03\",\"region\":\"West\",\"sales\":null}\n";

const CHARTS: [&str; 12] = [
    "line",
    "area",
    "stacked_area",
    "bar",
    "stacked_bar",
    "horizontal_bar",
    "scatter",
    "histogram",
    "box",
    "pie",
    "heatmap",
    "combo",
];

fn request_for(chart: &str, path: &str) -> String {
    let extra = match chart {
        "scatter" => r#","x":"sales","y":["margin"]"#,
        "combo" => r#","y":["sales"],"line":["margin"]"#,
        "heatmap" => r#","x":"region","y":["day"],"value":"sales""#,
        "histogram" => r#","x":"sales""#,
        _ => "",
    };
    format!(
        r#"{{"chart":"{chart}","title":"Fuzz","output":"svg","path":{}{extra}}}"#,
        json!(path)
    )
}

#[test]
fn corrupted_data_files_never_panic_and_always_answer_in_one_sentence() {
    let mut rng = Rng(0x00DE_C0DE_0001);
    let dir = TempDir::new();
    for (name, base) in [
        ("a.csv", CSV),
        ("b.json", JSON_ROWS),
        ("c.json", JSON_OBJECTS),
        ("d.jsonl", NDJSON),
    ] {
        for round in 0..150 {
            let bytes = mutate(base, &mut rng);
            let path = dir.write(name, &bytes);
            let chart = CHARTS[round % CHARTS.len()];
            check(&request_for(chart, path.to_str().unwrap()), false);
        }
    }
}

#[test]
fn corrupted_files_with_a_misleading_name_are_judged_by_their_content() {
    let mut rng = Rng(0x00DE_C0DE_0002);
    let dir = TempDir::new();
    for round in 0..120 {
        let base = [CSV, JSON_ROWS, JSON_OBJECTS, NDJSON][round % 4];
        let name = [
            "x.png", "x.csv", "x.json", "x.jsonl", "x", "x.tsv", "x.zip", "x.pdf",
        ][round % 8];
        let path = dir.write(name, &mutate(base, &mut rng));
        check(&request_for("line", path.to_str().unwrap()), false);
    }
}

#[test]
fn corrupted_requests_never_panic_either() {
    let mut rng = Rng(0x00DE_C0DE_0003);
    let (mut ok, mut failed) = (0, 0);
    for chart in CHARTS {
        let base = format!(
            r#"{{"chart":"{chart}","title":"T","x":"day","y":["sales"],"series":"region","legend":"bottom","theme":"dark","width":400,"height":300,"format":",.0f","x_scale":"auto","sort":"x","data":{{"columns":["day","region","sales","margin"],"rows":[["2024-01-01","N",1,2],["2024-01-02","S",3,4],["2024-01-03","N",5,6]]}}}}"#
        );
        for _ in 0..120 {
            let bytes = mutate(base.as_bytes(), &mut rng);
            if check(&String::from_utf8_lossy(&bytes), false) {
                ok += 1;
            } else {
                failed += 1;
            }
        }
    }
    assert!(
        ok >= 30 && failed >= 100,
        "{ok} succeeded and {failed} failed of 1440"
    );
}

#[test]
fn requests_with_wrongly_typed_or_extreme_field_values_are_answered_in_a_sentence() {
    let mut rng = Rng(0x00DE_C0DE_0006);
    let values: Vec<Value> = vec![
        json!(null),
        json!(true),
        json!(false),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(4096),
        json!(1e300),
        json!(""),
        json!("x"),
        json!("log"),
        json!("dark"),
        json!("sales"),
        json!("region"),
        json!("\u{1F4A5}"),
        json!([]),
        json!([1]),
        json!(["sales", "margin"]),
        json!({}),
        json!({"svg": "a.svg"}),
        json!("a,b\n1,2\n"),
    ];
    let keys = [
        "chart",
        "data",
        "x",
        "y",
        "line",
        "series",
        "size",
        "value",
        "agg",
        "sort",
        "stack",
        "horizontal",
        "bins",
        "title",
        "subtitle",
        "x_label",
        "y_label",
        "y2_label",
        "x_scale",
        "y_scale",
        "y2_scale",
        "format",
        "x_format",
        "y_format",
        "y2_format",
        "legend",
        "theme",
        "colors",
        "line_axis",
        "width",
        "height",
        "scale",
        "x_min",
        "x_max",
        "y_min",
        "y_max",
        "center",
        "output",
    ];
    let (mut ok, mut failed) = (0, 0);
    for round in 0..900 {
        let chart = CHARTS[round % CHARTS.len()];
        let mut req = json!({
            "chart": chart, "output": "svg", "x": "day", "y": ["sales"], "width": 320, "height": 240,
            "data": {"columns": ["day", "region", "sales", "margin"], "rows": [["2024-01-01", "N", 1, 2], ["2024-01-02", "S", 3, 4], ["2024-01-03", "N", 5, 6]]},
        });
        for _ in 0..=rng.below(3) {
            req[keys[rng.below(keys.len())]] = values[rng.below(values.len())].clone();
        }
        if check(&req.to_string(), false) {
            ok += 1;
        } else {
            failed += 1;
        }
    }
    assert!(
        ok >= 30 && failed >= 100,
        "{ok} succeeded and {failed} failed of 900"
    );
}

#[test]
fn some_corrupted_runs_are_checked_end_to_end_through_the_png() {
    let mut rng = Rng(0x00DE_C0DE_0004);
    let dir = TempDir::new();
    for round in 0..40 {
        let path = dir.write("f.csv", &mutate(CSV, &mut rng));
        let req = request_for(CHARTS[round % CHARTS.len()], path.to_str().unwrap()).replace(
            r#""output":"svg""#,
            r#""output":"both","width":300,"height":240"#,
        );
        check(&req, true);
    }
}

#[test]
fn awkward_numbers_do_not_break_any_chart_type() {
    let big = f64::MAX;
    let tiny = 5e-324;
    let sets: [Vec<f64>; 8] = [
        vec![big, big, big],
        vec![-big, big],
        vec![tiny, tiny * 2.0, 0.0],
        vec![1e300, 1e-300, 1.0],
        vec![0.0, 0.0, 0.0],
        vec![-5.0, -5.0, -5.0],
        vec![7.0],
        vec![1e15, 1e15 + 1.0, 1e15 + 2.0],
    ];
    for chart in CHARTS {
        for vs in &sets {
            let rows: Vec<Value> = vs
                .iter()
                .enumerate()
                .map(|(i, v)| json!([format!("k{i}"), v, v]))
                .collect();
            let extra = match chart {
                "scatter" => r#""x":"a","y":["b"]"#,
                "combo" => r#""y":["a"],"line":["b"]"#,
                "heatmap" => r#""x":"k","y":["a"],"value":"b""#,
                "histogram" => r#""x":"a""#,
                _ => r#""x":"k""#,
            };
            let req = with_rows(
                chart,
                &format!(r#"{extra},"output":"svg""#),
                &["k", "a", "b"],
                &rows,
            );
            check(&req, false);
        }
    }
}

#[test]
fn constant_and_single_row_data_draws_every_chart_type() {
    for chart in CHARTS {
        for rows in [
            vec![json!(["a", 5, 5])],
            vec![json!(["a", 5, 5]), json!(["b", 5, 5]), json!(["c", 5, 5])],
            vec![json!(["a", -1, -1]), json!(["b", -1, -1])],
        ] {
            let extra = match chart {
                "scatter" => r#""x":"a","y":["b"]"#,
                "combo" => r#""y":["a"],"line":["b"]"#,
                "heatmap" => r#""x":"k","y":["a"],"value":"b""#,
                "histogram" => r#""x":"a""#,
                "pie" => r#""x":"k","y":["a"]"#,
                _ => r#""x":"k""#,
            };
            let req = with_rows(chart, extra, &["k", "a", "b"], &rows);
            match crate::run(&req) {
                Ok(out) => {
                    let v: Value = serde_json::from_str(&out).unwrap();
                    parse(v["response"]["svg"].as_str().unwrap());
                }
                Err(e) => {
                    // A pie of negative values has nothing to show; every other case must draw.
                    assert!(chart == "pie" && e.0.contains("positive"), "{chart}: {e}");
                }
            }
        }
    }
}

#[test]
fn the_smallest_canvas_either_draws_or_says_it_is_too_small() {
    for chart in CHARTS {
        for (title, legend) in [
            ("", "none"),
            ("A title", "right"),
            (
                "A title that is rather long for such a small canvas",
                "bottom",
            ),
        ] {
            let extra = match chart {
                "scatter" => r#""x":"a","y":["b"]"#,
                "combo" => r#""y":["a"],"line":["b"]"#,
                "heatmap" => r#""x":"k","y":["a"],"value":"b""#,
                "histogram" => r#""x":"a""#,
                _ => r#""x":"k""#,
            };
            let req = with_rows(
                chart,
                &format!(
                    r#"{extra},"width":200,"height":150,"title":{},"legend":"{legend}""#,
                    json!(title)
                ),
                &["k", "a", "b"],
                &[json!(["x", 1, 2]), json!(["y", 3, 4]), json!(["z", 5, 6])],
            );
            match crate::run(&req) {
                Ok(_) => {}
                Err(e) => assert!(e.0.contains("too small"), "{chart}: {e}"),
            }
        }
    }
}

#[test]
fn the_largest_allowed_image_renders_within_the_memory_and_output_limits() {
    let rows: Vec<Value> = (0..200).map(|i| json!([i, (i * 7) % 90])).collect();
    let req = with_rows("line", r#""width":4000,"height":4000"#, &["x", "y"], &rows);
    let started = Instant::now();
    let out = crate::run(&req).unwrap();
    assert!(out.len() <= crate::output::BUDGET, "{} bytes", out.len());
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["response"]["png"]["width"], 4000);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_flood_of_series_and_categories_is_capped_not_drawn_in_full() {
    let mut csv = String::from("k");
    for s in 0..60 {
        csv.push_str(&format!(",s{s}"));
    }
    csv.push('\n');
    for r in 0..300 {
        csv.push_str(&format!("c{r}"));
        for s in 0..60 {
            csv.push_str(&format!(",{}", (r * s) % 17));
        }
        csv.push('\n');
    }
    for chart in ["line", "bar", "stacked_bar", "area", "stacked_area", "box"] {
        let v: Value = serde_json::from_str(
            &crate::run(&json!({"chart": chart, "output": "svg", "data": csv}).to_string())
                .unwrap(),
        )
        .unwrap();
        assert!(
            v["response"]["series"].as_array().unwrap().len() <= 40,
            "{chart}"
        );
        let w = v["response"]["warnings"].to_string();
        assert!(w.contains("only the first"), "{chart}: {w}");
    }
}

#[test]
fn hostile_file_and_save_names_cannot_escape_or_crash() {
    let outer = TempDir::new();
    let inner = outer.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(inner.join("data.csv"), CSV).unwrap();
    let dir = json!(inner.to_str().unwrap());
    let mut rng = Rng(0x00DE_C0DE_0005);
    let mut names: Vec<String> = [
        "..",
        ".",
        "",
        "/",
        "//",
        "a/../..",
        "./../x",
        "data.csv/",
        "data.csv/..",
        "con",
        "aux.csv",
        "~",
        "$HOME",
        "%00",
        "a\\b",
        "a:b",
        "x".repeat(5000).as_str(),
        "\u{202e}data.csv",
        "data.csv\0",
        "\n",
        " data.csv ",
        "d\u{e9}j\u{e0}.csv",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for _ in 0..200 {
        let len = rng.below(30);
        names.push(
            (0..len)
                .map(|_| ['.', '/', 'a', '\\', '\0', ' ', 'c', 's', 'v', ':', '~'][rng.below(11)])
                .collect(),
        );
    }
    for name in &names {
        let read = format!(
            r#"{{"chart":"bar","output":"svg","path":{dir},"file":{}}}"#,
            json!(name)
        );
        check(&read, false);
        let save = format!(
            r#"{{"chart":"bar","output":"svg","path":{dir},"file":"data.csv","save":{}}}"#,
            json!(name)
        );
        check(&save, false);
        let save_files = format!(
            r#"{{"chart":"bar","output":"svg","path":{dir},"file":"data.csv","save":{{"svg":{},"png":{}}}}}"#,
            json!(name),
            json!(name)
        );
        check(&save_files, false);
    }
    // Whatever was written is below the bound folder, and nothing next to it.
    let mut stack = vec![outer.path().to_path_buf()];
    while let Some(p) = stack.pop() {
        for e in std::fs::read_dir(&p).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.parent() == Some(outer.path()) {
                panic!("a file escaped to {path:?}");
            }
        }
    }
}

#[test]
fn deeply_nested_and_oversized_json_is_refused_in_a_sentence() {
    let dir = TempDir::new();
    let deep = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
    let p = dir.write("deep.json", deep.as_bytes());
    check(&request_for("line", p.to_str().unwrap()), false);
    let nested = format!(
        "{{\"data\":{}1{}}}",
        "{\"data\":".repeat(10_000),
        "}".repeat(10_000)
    );
    let p = dir.write("nested.json", nested.as_bytes());
    check(&request_for("line", p.to_str().unwrap()), false);
    let wide = format!(
        "[{{{}}}]",
        (0..5000)
            .map(|i| format!("\"c{i}\":{i}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let p = dir.write("wide.json", wide.as_bytes());
    check(&request_for("line", p.to_str().unwrap()), false);
}

#[test]
fn a_file_with_a_single_endless_line_is_refused_not_buffered() {
    let dir = TempDir::new();
    let mut body = b"a,b\n".to_vec();
    body.extend(std::iter::repeat_n(b'9', 5_000_000));
    let p = dir.write("line.csv", &body);
    let e = crate::run(&request_for("line", p.to_str().unwrap()))
        .unwrap_err()
        .0;
    assert!(e.contains("longer than 65536 bytes"), "{e}");
}
