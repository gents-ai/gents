//! Paging, cursors and the properties that hold for every result: pages join
//! into the one-shot result, SQL agrees with a tiny reference evaluation, an
//! export reads back identically, and a clean integer column never turns float.
#![cfg(test)]
use proptest::prelude::*;
use serde_json::{json, Value};

use crate::testkit::{rows, run, Dir};

/// Every page of `sql`, following the cursor; the rows joined, and how many pages there were.
fn all_pages(dir: &Dir, sql: &str, max_rows: usize) -> (Vec<Value>, usize) {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut input = json!({"path": dir.s(), "sql": sql, "max_rows": max_rows});
        if let Some(c) = &cursor {
            input["cursor"] = json!(c);
        }
        let r = run(input).unwrap_or_else(|e| panic!("{sql}: {e}"));
        pages += 1;
        assert!(rows(&r).len() <= max_rows);
        assert_eq!(r["offset"], out.len());
        out.extend(rows(&r));
        match r["next"]["cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => return (out, pages),
        }
        assert!(pages < 100_000, "paging never ended");
    }
}

fn numbers(n: usize) -> Dir {
    let d = Dir::new();
    let mut text = String::from("n,g,pad\n");
    for i in 0..n {
        text.push_str(&format!("{i},{},{}\n", i % 7, "x".repeat(i % 5)));
    }
    d.put("t.csv", text);
    d
}

#[test]
fn pages_join_into_the_one_shot_result_for_every_query_shape_and_size() {
    let d = numbers(1000);
    for sql in [
        "SELECT * FROM t",
        "SELECT n, g FROM t WHERE g <> 3",
        "SELECT g, count(*) AS c, sum(n) AS s FROM t GROUP BY g",
        "SELECT * FROM t ORDER BY g",
        "SELECT * FROM t ORDER BY pad DESC, g",
        "SELECT DISTINCT g, pad FROM t",
        "SELECT n, row_number() OVER (PARTITION BY g ORDER BY n DESC) AS rn FROM t",
        "SELECT * FROM t LIMIT 333",
        "SELECT a.n, b.g FROM t a JOIN t b ON a.n = b.n WHERE a.n < 400",
    ] {
        let (whole, one) = all_pages(&d, sql, 100_000);
        assert_eq!(one, 1, "{sql}");
        assert!(!whole.is_empty());
        for size in [1usize, 7, 64, 333, 999, 1000, 1001, 5000] {
            let (joined, pages) = all_pages(&d, sql, size);
            assert_eq!(joined.len(), whole.len(), "{sql} size {size}");
            assert_eq!(joined, whole, "{sql} size {size}");
            assert_eq!(pages, whole.len().div_ceil(size).max(1), "{sql} size {size}");
        }
    }
}

#[test]
fn a_result_that_fills_a_page_exactly_has_no_empty_page_after_it() {
    let d = numbers(10);
    let r = run(json!({"path": d.s(), "sql": "SELECT n FROM t", "max_rows": 10})).unwrap();
    assert_eq!(rows(&r).len(), 10);
    assert!(r.get("next").is_none());
    let r = run(json!({"path": d.s(), "sql": "SELECT n FROM t", "max_rows": 9})).unwrap();
    assert!(r["next"]["cursor"].is_string());
    let r = run(json!({"path": d.s(), "sql": "SELECT n FROM t WHERE n > 100", "max_rows": 9})).unwrap();
    assert_eq!((rows(&r).len(), r.get("next").is_none(), r["markdown"].as_str().unwrap()), (0, true, "| n |\n| --- |"));
}

#[test]
fn the_byte_budget_ends_a_page_and_the_pages_still_join() {
    let d = Dir::new();
    let mut text = String::from("id,blob\n");
    for i in 0..40 {
        text.push_str(&format!("{i},{}\n", "z".repeat(1500)));
    }
    d.put("t.csv", text);
    let whole = run(json!({"path": d.s(), "sql": "SELECT * FROM t"})).unwrap();
    assert_eq!(rows(&whole).len(), 40);
    let mut joined = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut input = json!({"path": d.s(), "sql": "SELECT * FROM t", "max_bytes": 4096});
        if let Some(c) = &cursor {
            input["cursor"] = json!(c);
        }
        let r = run(input).unwrap();
        pages += 1;
        let bytes: usize = rows(&r).iter().map(|row| row.to_string().len()).sum();
        // A page stops at the first row that would pass the budget, so it overshoots by less than a row.
        assert!(bytes < 4096 + 1600, "{bytes}");
        assert!(!rows(&r).is_empty());
        joined.extend(rows(&r));
        match r["next"]["cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(joined, rows(&whole));
    assert!(pages > 5, "{pages}");
    // One row bigger than the whole budget still comes back, alone.
    let r = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_bytes": 4096, "max_rows": 1})).unwrap();
    assert_eq!(rows(&r).len(), 1);
}

#[test]
fn a_cursor_is_refused_when_the_query_the_data_or_the_options_changed() {
    let d = numbers(50);
    let first = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 10})).unwrap();
    let cursor = first["next"]["cursor"].as_str().unwrap().to_string();
    let call = |sql: &str, cursor: &str| run(json!({"path": d.s(), "sql": sql, "cursor": cursor, "max_rows": 10}));
    assert_eq!(rows(&call("SELECT * FROM t", &cursor).unwrap())[0][0], 10);
    for other in ["SELECT * FROM t WHERE n > 0", "select * from t", "SELECT n FROM t"] {
        let e = call(other, &cursor).unwrap_err();
        assert!(e.contains("different query or the data changed"), "{other}: {e}");
    }
    let e = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "cursor": cursor, "delimiter": ";"})).unwrap_err();
    assert!(e.contains("different query or the data changed") || e.contains("not valid"), "{e}");
    d.put("t.csv", "n,g,pad\n1,1,x\n");
    let e = call("SELECT * FROM t", &cursor).unwrap_err();
    assert!(e.contains("different query or the data changed"), "{e}");
    for junk in ["", "abc", "dt1.x.y", &format!("{cursor}0")] {
        let e = call("SELECT * FROM t", junk).unwrap_err();
        assert!(e.contains("cursor is not valid"), "{junk}: {e}");
    }
}

#[test]
fn a_cursor_may_change_the_page_size_but_not_the_rows_it_continues_from() {
    let d = numbers(100);
    let first = run(json!({"path": d.s(), "sql": "SELECT n FROM t", "max_rows": 10})).unwrap();
    let cursor = first["next"]["cursor"].as_str().unwrap();
    let next = run(json!({"path": d.s(), "sql": "SELECT n FROM t", "max_rows": 25, "cursor": cursor})).unwrap();
    assert_eq!(rows(&next).len(), 25);
    assert_eq!(rows(&next)[0], json!([10]));
    assert_eq!(next["offset"], 10);
}

#[test]
fn the_unordered_shapes_come_back_in_one_repeatable_order() {
    let d = numbers(300);
    for sql in ["SELECT g, count(*) AS c FROM t GROUP BY g", "SELECT a.n FROM t a JOIN t b ON a.g = b.g AND b.n < 5", "SELECT DISTINCT pad FROM t"] {
        let a = run(json!({"path": d.s(), "sql": sql})).unwrap();
        let b = run(json!({"path": d.s(), "sql": sql})).unwrap();
        assert_eq!(a, b, "{sql}");
        assert_eq!(a["order"], "columns");
        let first_column: Vec<Value> = rows(&a).iter().map(|r| r[0].clone()).collect();
        let mut sorted = first_column.clone();
        sorted.sort_by(|x, y| x.to_string().len().cmp(&y.to_string().len()).then(x.to_string().cmp(&y.to_string())));
        if first_column.iter().all(Value::is_number) {
            let mut nums: Vec<i64> = first_column.iter().map(|v| v.as_i64().unwrap()).collect();
            let copy = nums.clone();
            nums.sort();
            assert_eq!(copy, nums, "{sql}");
        }
    }
}

#[derive(Debug, Clone)]
struct Row {
    a: i32,
    b: i32,
    c: &'static str,
}

fn table_of(rows: &[Row]) -> Dir {
    let d = Dir::new();
    let mut text = String::from("a,b,c\n");
    for r in rows {
        text.push_str(&format!("{},{},{}\n", r.a, r.b, r.c));
    }
    d.put("t.csv", text);
    d
}

fn arbitrary_rows() -> impl Strategy<Value = Vec<Row>> {
    prop::collection::vec((-50i32..50, -50i32..50, prop::sample::select(vec!["x", "y", "z", "w"])).prop_map(|(a, b, c)| Row { a, b, c }), 1..40)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    #[test]
    fn pages_join_into_the_one_shot_result_for_random_tables_and_sizes(rows_in in arbitrary_rows(), size in 1usize..60, shape in 0usize..4) {
        let d = table_of(&rows_in);
        let sql = ["SELECT * FROM t", "SELECT c, count(*) AS n, sum(a) AS s FROM t GROUP BY c", "SELECT * FROM t ORDER BY c", "SELECT DISTINCT c, b FROM t"][shape];
        let (whole, _) = all_pages(&d, sql, 100_000);
        let (joined, _) = all_pages(&d, sql, size);
        prop_assert_eq!(joined, whole);
    }

    #[test]
    fn filters_and_aggregates_agree_with_a_plain_evaluation(rows_in in arbitrary_rows(), k in -50i32..50, m in -50i32..50) {
        let d = table_of(&rows_in);
        let kept: Vec<&Row> = rows_in.iter().filter(|r| r.a > k && r.b <= m).collect();

        let r = run(json!({"path": d.s(), "sql": format!("SELECT a, b FROM t WHERE a > {k} AND b <= {m}")})).unwrap();
        let want: Vec<Value> = kept.iter().map(|r| json!([r.a, r.b])).collect();
        prop_assert_eq!(rows(&r), want);

        let r = run(json!({"path": d.s(), "sql": format!("SELECT count(*) AS n, sum(a) AS s, min(b) AS lo, max(b) AS hi FROM t WHERE a > {k} AND b <= {m}")})).unwrap();
        let want = if kept.is_empty() {
            json!([[0, null, null, null]])
        } else {
            json!([[kept.len(), kept.iter().map(|r| i64::from(r.a)).sum::<i64>(), kept.iter().map(|r| r.b).min(), kept.iter().map(|r| r.b).max()]])
        };
        prop_assert_eq!(r["rows"].clone(), want);

        let r = run(json!({"path": d.s(), "sql": format!("SELECT c, count(*) AS n, sum(a) AS s FROM t WHERE a > {k} GROUP BY c")})).unwrap();
        let mut groups: std::collections::BTreeMap<&str, (usize, i64)> = Default::default();
        for row in rows_in.iter().filter(|r| r.a > k) {
            let e = groups.entry(row.c).or_default();
            e.0 += 1;
            e.1 += i64::from(row.a);
        }
        let want: Vec<Value> = groups.iter().map(|(c, (n, s))| json!([c, n, s])).collect();
        prop_assert_eq!(rows(&r), want);
    }

    #[test]
    fn an_integer_column_stays_integer_through_every_step(ns in prop::collection::vec(any::<i64>(), 1..30)) {
        let d = Dir::new();
        d.put("t.csv", format!("v\n{}", ns.iter().map(|n| format!("{n}\n")).collect::<String>()));
        let r = run(json!({"path": d.s(), "sql": "SELECT v FROM t"})).unwrap();
        prop_assert_eq!(r["columns"][0]["type"].clone(), json!("int64"));
        let back: Vec<i64> = rows(&r).iter().map(|row| match &row[0] {
            Value::Number(n) => n.as_i64().unwrap(),
            Value::String(s) => s.parse().unwrap(),
            other => panic!("{other}"),
        }).collect();
        prop_assert_eq!(back, ns);
    }
}

#[derive(Debug, Clone)]
enum Col {
    Int(Vec<Option<i64>>),
    Float(Vec<Option<i32>>),
    Text(Vec<Option<String>>),
    Bool(Vec<Option<bool>>),
    Date(Vec<Option<u32>>),
}

fn csv_cell(s: Option<String>) -> String {
    match s {
        None => String::new(),
        Some(s) if s.is_empty() => "\"\"".into(),
        Some(s) if s.contains([',', '"', '\n', '\r']) => format!("\"{}\"", s.replace('"', "\"\"")),
        Some(s) => s,
    }
}

fn arbitrary_table() -> impl Strategy<Value = Vec<Col>> {
    (1usize..12).prop_flat_map(|n| {
        prop::collection::vec(
            prop_oneof![
                prop::collection::vec(prop::option::of(any::<i64>()), n).prop_map(Col::Int),
                prop::collection::vec(prop::option::of(-100_000i32..100_000), n).prop_map(Col::Float),
                prop::collection::vec(prop::option::of("[ -~\n\r]{0,10}"), n).prop_map(Col::Text),
                prop::collection::vec(prop::option::of(any::<bool>()), n).prop_map(Col::Bool),
                prop::collection::vec(prop::option::of(0u32..30_000), n).prop_map(Col::Date),
            ],
            2..6,
        )
    })
}

fn render_csv(cols: &[Col]) -> String {
    let n = match &cols[0] {
        Col::Int(v) => v.len(),
        Col::Float(v) => v.len(),
        Col::Text(v) => v.len(),
        Col::Bool(v) => v.len(),
        Col::Date(v) => v.len(),
    };
    let mut out = (0..cols.len()).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",") + "\n";
    for r in 0..n {
        let cells: Vec<String> = cols
            .iter()
            .map(|c| {
                csv_cell(match c {
                    Col::Int(v) => v[r].map(|x| x.to_string()),
                    Col::Float(v) => v[r].map(|x| format!("{:.2}", f64::from(x) / 100.0)),
                    Col::Text(v) => v[r].clone(),
                    Col::Bool(v) => v[r].map(|x| x.to_string()),
                    Col::Date(v) => v[r].map(|d| {
                        let date = chrono::NaiveDate::from_num_days_from_ce_opt(719_163 + d as i32).unwrap();
                        date.format("%Y-%m-%d").to_string()
                    }),
                })
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    #[test]
    fn a_csv_queried_exported_and_queried_again_gives_identical_rows_and_types(table in arbitrary_table()) {
        let d = Dir::new();
        d.put("t.csv", render_csv(&table));
        let first = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 100_000})).unwrap();
        let exported = run(json!({"path": d.s(), "mode": "export", "sql": "SELECT * FROM t", "output": "out.csv"})).unwrap();
        prop_assert_eq!(exported["rows"].clone(), first["row_count"].clone());
        let second = run(json!({"path": d.s(), "sql": "SELECT * FROM out", "max_rows": 100_000})).unwrap();
        prop_assert_eq!(&second["columns"], &first["columns"]);
        prop_assert_eq!(&second["rows"], &first["rows"]);
        // And through Parquet.
        run(json!({"path": d.s(), "mode": "export", "sql": "SELECT * FROM t", "output": "out.parquet"})).unwrap();
        let third = run(json!({"path": d.s(), "sql": "SELECT * FROM out_2", "max_rows": 100_000})).unwrap();
        prop_assert_eq!(&third["columns"], &first["columns"]);
        prop_assert_eq!(&third["rows"], &first["rows"]);
    }
}
