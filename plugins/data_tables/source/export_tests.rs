//! The `export` mode: what is written, what is refused, and that a failure
//! leaves nothing behind.
#![cfg(test)]
use serde_json::json;

use crate::testkit::{fixtures, query, rows, run, Dir};
use parquet::basic::Compression;

const SALES: &str = "region,units,price,day\nnorth,10,2.5,2024-01-01\nsouth,7,3,2024-01-02\nnorth,5,4,2024-01-03\neast,,1.5,2024-01-04\nsouth,3,2,2024-01-05\n";

fn export(d: &Dir, extra: serde_json::Value) -> Result<serde_json::Value, String> {
    let mut input = json!({"path": d.s(), "mode": "export", "sql": "SELECT * FROM sales ORDER BY day"});
    for (k, v) in extra.as_object().unwrap() {
        input[k] = v.clone();
    }
    run(input)
}

fn sales() -> Dir {
    let d = Dir::new();
    d.put("sales.csv", SALES);
    d
}

#[test]
fn csv_is_written_exactly_with_null_and_floats_kept_apart() {
    let d = sales();
    let r = export(&d, json!({"output": "out.csv"})).unwrap();
    let text = std::fs::read_to_string(d.path().join("out.csv")).unwrap();
    assert_eq!(text, "region,units,price,day\nnorth,10,2.5,2024-01-01\nsouth,7,3.0,2024-01-02\nnorth,5,4.0,2024-01-03\neast,,1.5,2024-01-04\nsouth,3,2.0,2024-01-05\n");
    assert_eq!(r, json!({"written": "out.csv", "format": "csv", "rows": 5, "bytes": text.len(), "warnings": []}));
}

#[test]
fn csv_quotes_what_needs_it_and_keeps_the_empty_string() {
    let d = Dir::new();
    let r = run(json!({"path": d.s(), "mode": "export", "output": "o.csv", "tables": {"t": {"rows": [{"a": "x,y", "b": "say \"hi\"", "c": "", "d": null, "e": "l1\nl2"}]}}, "sql": "SELECT * FROM t"}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(std::fs::read_to_string(d.path().join("o.csv")).unwrap(), "a,b,c,d,e\n\"x,y\",\"say \"\"hi\"\"\",\"\",,\"l1\nl2\"\n");
}

#[test]
fn a_one_column_null_is_written_as_an_empty_string_and_said() {
    let d = Dir::new();
    let r = run(json!({"path": d.s(), "mode": "export", "output": "o.csv", "tables": {"t": {"rows": [["a"], [null], ["b"]]}}, "sql": "SELECT * FROM t"})).unwrap();
    assert_eq!(std::fs::read_to_string(d.path().join("o.csv")).unwrap(), "column_1\na\n\"\"\nb\n");
    assert_eq!(r["warnings"], json!(["1 NULL values in a one-column file were written as empty strings, because a blank line is not a row"]));
    assert_eq!(r["rows"], 3);
}

#[test]
fn parquet_keeps_exact_types_and_reads_back_identically() {
    let d = Dir::new();
    d.put("p.parquet", fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY));
    let before = query(&d, "SELECT * FROM p").unwrap();
    let r = run(json!({"path": d.s(), "mode": "export", "sql": "SELECT * FROM p", "output": "copy.parquet"})).unwrap();
    assert_eq!((r["rows"].clone(), r["format"].clone()), (json!(3), json!("parquet")));
    let after = query(&d, "SELECT * FROM copy").unwrap();
    assert_eq!(after["columns"], before["columns"]);
    assert_eq!(after["rows"], before["rows"]);
    assert_eq!(&std::fs::read(d.path().join("copy.parquet")).unwrap()[..4], b"PAR1");
}

#[test]
fn the_format_comes_from_the_name_or_is_given_and_must_be_known() {
    let d = sales();
    assert_eq!(export(&d, json!({"output": "a.csv"})).unwrap()["format"], "csv");
    assert_eq!(export(&d, json!({"output": "a.parquet"})).unwrap()["format"], "parquet");
    assert_eq!(export(&d, json!({"output": "plain", "format": "csv"})).unwrap()["format"], "csv");
    assert!(d.path().join("plain").exists());
    assert_eq!(export(&d, json!({"output": "x.dat"})).unwrap_err(), "format is required unless output ends in .csv or .parquet");
    assert!(run(json!({"path": d.s(), "mode": "export", "sql": "SELECT 1", "output": "x.csv", "format": "xlsx"})).is_err());
}

#[test]
fn an_existing_file_is_not_replaced_unless_asked_and_nothing_else_changes() {
    let d = sales();
    d.put("keep.csv", "old\n");
    let e = export(&d, json!({"output": "keep.csv"})).unwrap_err();
    assert_eq!(e, "keep.csv already exists; choose another output name or set overwrite to true");
    assert_eq!(std::fs::read_to_string(d.path().join("keep.csv")).unwrap(), "old\n");
    export(&d, json!({"output": "keep.csv", "overwrite": true})).unwrap();
    assert!(std::fs::read_to_string(d.path().join("keep.csv")).unwrap().starts_with("region,units"));
    assert_eq!(export(&d, json!({"output": "keep.csv", "overwrite": false})).unwrap_err(), "keep.csv already exists; choose another output name or set overwrite to true");
}

#[test]
fn names_that_could_leave_the_folder_or_hide_are_refused() {
    let d = sales();
    for bad in ["../x.csv", "a/b.csv", ".hidden.csv", "", "a\\b.csv", "/abs.csv"] {
        let e = export(&d, json!({"output": bad})).unwrap_err();
        assert_eq!(e, "output must be a plain file name such as result.csv, without folders and not starting with a dot", "{bad:?}");
    }
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    let e = run(json!({"path": d.s(), "mode": "export", "sql": "SELECT 1"})).unwrap_err();
    assert_eq!(e, "output is required: the file name to write, such as result.csv");
    let e = run(json!({"path": d.s(), "mode": "export", "output": "x.csv"})).unwrap_err();
    assert_eq!(e, "sql is required: give the SELECT to run");
}

#[test]
fn a_file_or_missing_path_cannot_be_exported_into() {
    let d = sales();
    let p = d.path().join("sales.csv");
    let e = run(json!({"path": p.to_string_lossy(), "mode": "export", "sql": "SELECT * FROM sales", "output": "o.csv"})).unwrap_err();
    assert_eq!(e, "export writes into the bound folder: path must be a folder, not a file");
    let e = run(json!({"mode": "export", "sql": "SELECT 1", "output": "o.csv", "tables": {"t": [[1]]}})).unwrap_err();
    assert_eq!(e, "export writes into the bound folder: give path as a folder");
}

#[test]
fn a_failing_query_writes_nothing_and_leaves_no_temporary_file() {
    let d = sales();
    for sql in ["SELECT * FROM missing", "SELECT 1 / 0 AS x FROM sales", "DROP TABLE sales", "SELECT"] {
        let e = export(&d, json!({"output": "o.csv", "sql": sql}));
        assert!(e.is_err(), "{sql}");
        let names: Vec<String> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["sales.csv"], "{sql}");
    }
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_written_is_one_sentence() {
    use std::os::unix::fs::PermissionsExt;
    let d = sales();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let blocked = std::fs::write(d.path().join("probe"), "x").is_err();
    let e = export(&d, json!({"output": "o.csv"}));
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    if blocked {
        let e = e.unwrap_err();
        assert_eq!(e, "cannot create the output file: the folder is read-only for this call; export needs a tool call the user allowed to write there");
    }
}

#[test]
fn a_large_export_streams_and_reads_back_whole() {
    let d = Dir::new();
    let mut text = String::from("id,v\n");
    for i in 0..50_000 {
        text.push_str(&format!("{i},{}\n", i * 3));
    }
    d.put("big.csv", text);
    let r = run(json!({"path": d.s(), "mode": "export", "sql": "SELECT id, v + 1 AS w FROM big WHERE id % 2 = 0", "output": "half.parquet"})).unwrap();
    assert_eq!(r["rows"], 25_000);
    let back = query(&d, "SELECT count(*) AS n, min(w) AS lo, max(w) AS hi FROM half").unwrap();
    assert_eq!(back["rows"], json!([[25_000, 1, 149_995]]));
    assert_eq!(rows(&back).len(), 1);
}

#[test]
fn an_export_that_widens_types_midway_restarts_cleanly() {
    let d = Dir::new();
    let mut text = String::from("v\n");
    for i in 0..100_005 {
        text.push_str(&format!("{i}\n"));
    }
    text.push_str("late\n");
    d.put("t.csv", text);
    let r = run(json!({"path": d.s(), "mode": "export", "sql": "SELECT v FROM t", "output": "o.csv"})).unwrap();
    assert_eq!(r["rows"], 100_006);
    let written = std::fs::read_to_string(d.path().join("o.csv")).unwrap();
    assert_eq!(written.lines().count(), 100_007);
    assert!(written.ends_with("99999\nlate\n") || written.ends_with("100004\nlate\n"));
}
