//! Paging, cursors and the properties that hold for every result: pages join
//! into the one-shot result, SQL agrees with a tiny reference evaluation, an
//! export reads back identically, and a clean integer column never turns float.
#![cfg(test)]
use proptest::prelude::*;
use serde_json::{Value, json};

use crate::testkit::{Dir, rows, run};

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
            assert_eq!(
                pages,
                whole.len().div_ceil(size).max(1),
                "{sql} size {size}"
            );
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
    let r =
        run(json!({"path": d.s(), "sql": "SELECT n FROM t WHERE n > 100", "max_rows": 9})).unwrap();
    assert_eq!(
        (
            rows(&r).len(),
            r.get("next").is_none(),
            r["markdown"].as_str().unwrap()
        ),
        (0, true, "| n |\n| --- |")
    );
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
    let r = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_bytes": 4096, "max_rows": 1}))
        .unwrap();
    assert_eq!(rows(&r).len(), 1);
}

#[test]
fn a_cursor_is_refused_when_the_query_the_data_or_the_options_changed() {
    let d = numbers(50);
    let first = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 10})).unwrap();
    let cursor = first["next"]["cursor"].as_str().unwrap().to_string();
    let call = |sql: &str, cursor: &str| {
        run(json!({"path": d.s(), "sql": sql, "cursor": cursor, "max_rows": 10}))
    };
    assert_eq!(rows(&call("SELECT * FROM t", &cursor).unwrap())[0][0], 10);
    for other in [
        "SELECT * FROM t WHERE n > 0",
        "select * from t",
        "SELECT n FROM t",
    ] {
        let e = call(other, &cursor).unwrap_err();
        assert!(
            e.contains("different query or the data changed"),
            "{other}: {e}"
        );
    }
    let e =
        run(json!({"path": d.s(), "sql": "SELECT * FROM t", "cursor": cursor, "delimiter": ";"}))
            .unwrap_err();
    assert_eq!(
        e,
        "the cursor belongs to a different query or the data changed; start again without a cursor"
    );
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
    let next =
        run(json!({"path": d.s(), "sql": "SELECT n FROM t", "max_rows": 25, "cursor": cursor}))
            .unwrap();
    assert_eq!(rows(&next).len(), 25);
    assert_eq!(rows(&next)[0], json!([10]));
    assert_eq!(next["offset"], 10);
}

/// Orders JSON values the way the documented rule does: numbers by value, text bytewise,
/// NULL after everything.
fn value_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match (a, b) {
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Greater,
        (_, Value::Null) => Less,
        (Value::Number(x), Value::Number(y)) => {
            x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(Equal)
        }
        (Value::String(x), Value::String(y)) => x.as_bytes().cmp(y.as_bytes()),
        other => panic!("columns of mixed kinds: {other:?}"),
    }
}

fn row_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    let (a, b) = (a.as_array().unwrap(), b.as_array().unwrap());
    a.iter()
        .zip(b)
        .map(|(x, y)| value_cmp(x, y))
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

#[test]
fn the_unordered_shapes_come_back_in_one_repeatable_order() {
    let d = numbers(300);
    for sql in [
        "SELECT g, count(*) AS c FROM t GROUP BY g",
        "SELECT a.n FROM t a JOIN t b ON a.g = b.g AND b.n < 5",
        // `pad` is text, empty in every fifth row, and an empty unquoted field is NULL.
        "SELECT DISTINCT pad FROM t",
        "SELECT pad, g, count(*) AS c FROM t GROUP BY pad, g",
    ] {
        let a = run(json!({"path": d.s(), "sql": sql})).unwrap();
        let b = run(json!({"path": d.s(), "sql": sql})).unwrap();
        assert_eq!(a, b, "{sql}");
        assert_eq!(a["order"], "columns");
        // Against a sort done here, column by column, ascending, NULLs last.
        let mut want = rows(&a);
        want.sort_by(row_cmp);
        assert_eq!(rows(&a), want, "{sql}");
    }
    let r = run(json!({"path": d.s(), "sql": "SELECT DISTINCT pad FROM t"})).unwrap();
    assert_eq!(
        rows(&r),
        [
            json!(["x"]),
            json!(["xx"]),
            json!(["xxx"]),
            json!(["xxxx"]),
            json!([null])
        ]
    );
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
    prop::collection::vec(
        (
            -50i32..50,
            -50i32..50,
            prop::sample::select(vec!["x", "y", "z", "w"]),
        )
            .prop_map(|(a, b, c)| Row { a, b, c }),
        1..40,
    )
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

/// Whether a result carries no warning about rows that were padded or values that were lost.
fn nothing_lost(r: &Value) -> bool {
    r["warnings"].as_array().unwrap().iter().all(|w| {
        let w = w.as_str().unwrap();
        !w.contains("lost their extra values") && !w.contains("padded with NULL")
    })
}

/// One field the way a careful writer makes it: quoted when it holds any delimiter a reader
/// may sniff, a quote or a line break.
fn csv_cell(s: Option<String>) -> String {
    match s {
        None => String::new(),
        Some(s) if s.is_empty() => "\"\"".into(),
        Some(s) if s.contains([',', ';', '|', '\t', '"', '\n', '\r']) => {
            format!("\"{}\"", s.replace('"', "\"\""))
        }
        Some(s) => s,
    }
}

/// Text that is often one of the delimiters a reader sniffs, or made of nothing else.
fn tricky_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[ -~\t\n\r]{0,10}",
        2 => prop::sample::select(vec![
            "a;b", "c;d", "e;f", "a|b", "x\ty", "a,b;c", ";", "|", "\t", ";;;", "a;b;c|d",
        ])
        .prop_map(String::from),
    ]
}

fn arbitrary_table() -> impl Strategy<Value = Vec<Col>> {
    (1usize..12).prop_flat_map(|n| {
        // From one column (where a delimiter inside every value is easy to misread) to seven.
        prop::collection::vec(
            prop_oneof![
                prop::collection::vec(prop::option::of(any::<i64>()), n).prop_map(Col::Int),
                prop::collection::vec(prop::option::of(-100_000i32..100_000), n)
                    .prop_map(Col::Float),
                prop::collection::vec(prop::option::of(tricky_text()), n).prop_map(Col::Text),
                prop::collection::vec(prop::option::of(any::<bool>()), n).prop_map(Col::Bool),
                prop::collection::vec(prop::option::of(0u32..30_000), n).prop_map(Col::Date),
            ],
            1..8,
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
    let mut out = (0..cols.len())
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join(",")
        + "\n";
    for r in 0..n {
        let cells: Vec<String> = cols
            .iter()
            .map(|c| {
                let cell = match c {
                    Col::Int(v) => v[r].map(|x| x.to_string()),
                    Col::Float(v) => v[r].map(|x| format!("{:.2}", f64::from(x) / 100.0)),
                    Col::Text(v) => v[r].clone(),
                    Col::Bool(v) => v[r].map(|x| x.to_string()),
                    Col::Date(v) => v[r].map(|d| {
                        let date = chrono::NaiveDate::from_num_days_from_ce_opt(719_163 + d as i32)
                            .unwrap();
                        date.format("%Y-%m-%d").to_string()
                    }),
                };
                // A blank line is no row, so a one-column file cannot hold a NULL.
                csv_cell(if cols.len() == 1 && cell.is_none() {
                    Some(String::new())
                } else {
                    cell
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
        let exported = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "output": "out.csv"})).unwrap();
        // Nothing is lost on the way in or out.
        prop_assert!(nothing_lost(&first), "{:?}", first["warnings"]);
        prop_assert_eq!(exported["rows"].clone(), first["row_count"].clone());
        let second = run(json!({"path": d.s(), "sql": "SELECT * FROM out", "max_rows": 100_000})).unwrap();
        prop_assert_eq!(&second["columns"], &first["columns"]);
        prop_assert_eq!(&second["rows"], &first["rows"]);
        prop_assert!(nothing_lost(&second), "{:?}", second["warnings"]);
        // And through Parquet.
        run(json!({"path": d.s(), "sql": "SELECT * FROM t", "output": "out.parquet"})).unwrap();
        let third = run(json!({"path": d.s(), "sql": "SELECT * FROM out_2", "max_rows": 100_000})).unwrap();
        prop_assert_eq!(&third["columns"], &first["columns"]);
        prop_assert_eq!(&third["rows"], &first["rows"]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 60, ..ProptestConfig::default() })]

    /// Every text value comes back exactly, however many columns there are and whichever
    /// delimiters it holds: through the reader, and through an export and the reader again.
    #[test]
    fn text_with_delimiters_in_it_survives_a_read_and_an_export_exactly(
        table in (1usize..6).prop_flat_map(|cols| prop::collection::vec(
            prop::collection::vec(tricky_text().prop_map(|t| format!("v{t}")), cols),
            1..8,
        )),
        names in any::<bool>(),
    ) {
        let width = table[0].len();
        let header: Vec<String> = (0..width)
            .map(|i| if names { format!("n{i};x|y") } else { format!("c{i}") })
            .collect();
        let mut text = header.iter().map(|h| csv_cell(Some(h.clone()))).collect::<Vec<_>>().join(",") + "\n";
        for row in &table {
            text.push_str(&row.iter().map(|v| csv_cell(Some(v.clone()))).collect::<Vec<_>>().join(","));
            text.push('\n');
        }
        let d = Dir::new();
        d.put("t.csv", &text);
        let want: Vec<Value> = table.iter().map(|r| json!(r)).collect();
        let columns = |r: &Value| -> Vec<String> {
            r["columns"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect()
        };
        let first = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 100_000})).unwrap();
        prop_assert_eq!(rows(&first), want.clone());
        prop_assert_eq!(columns(&first), header.clone());
        prop_assert_eq!(&first["warnings"], &json!([]));
        run(json!({"path": d.s(), "sql": "SELECT * FROM t", "output": "out.csv"})).unwrap();
        let second = run(json!({"path": d.s(), "sql": "SELECT * FROM out", "max_rows": 100_000})).unwrap();
        prop_assert_eq!(rows(&second), want);
        prop_assert_eq!(columns(&second), header);
        prop_assert_eq!(&second["warnings"], &json!([]));
    }
}

#[test]
fn a_cursor_is_refused_when_a_byte_in_the_middle_of_a_file_changed_and_its_size_did_not() {
    let d = numbers(30_000);
    let before = std::fs::read(d.path().join("t.csv")).unwrap();
    assert!(before.len() > 128 * 1024, "{}", before.len());
    let first = run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 10})).unwrap();
    let cursor = first["next"]["cursor"].as_str().unwrap();
    let again = |c: &str| {
        run(json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 10, "cursor": c}))
    };
    assert_eq!(again(cursor).unwrap()["offset"], 10);
    // One digit in the middle becomes another digit: same size, different rows.
    let mut after = before.clone();
    let at = after.len() / 2;
    let digit = after[at..].iter().position(u8::is_ascii_digit).unwrap() + at;
    after[digit] = if after[digit] == b'7' { b'8' } else { b'7' };
    assert_eq!(after.len(), before.len());
    d.put("t.csv", &after);
    assert_eq!(
        again(cursor).unwrap_err(),
        "the cursor belongs to a different query or the data changed; start again without a cursor"
    );
}

#[test]
fn every_page_of_a_chain_has_the_same_column_types_across_the_sample_boundary() {
    // 100,500 rows; the one text value comes at row 100,300, past the 100,000-row type sample,
    // so a sampled read says int64 and only a read of the whole column says text.
    let d = Dir::new();
    let mut text = String::from("x,y\n");
    for i in 0..100_500 {
        let x = if i == 100_300 {
            "abc".to_string()
        } else {
            i.to_string()
        };
        text.push_str(&format!("{x},{i}\n"));
    }
    d.put("t.csv", text);
    let sql = "SELECT x, y FROM t";
    let one_shot = run(json!({"path": d.s(), "sql": sql, "max_rows": 100_000})).unwrap();
    let mut cursor: Option<String> = None;
    let (mut pages, mut seen) = (0, 0usize);
    loop {
        let mut input = json!({"path": d.s(), "sql": sql, "max_rows": 40_000});
        if let Some(c) = &cursor {
            input["cursor"] = json!(c);
        }
        let r = run(input).unwrap();
        // The type of the first page is the type of the last.
        assert_eq!(
            r["columns"],
            json!([{"name": "x", "type": "text"}, {"name": "y", "type": "int64"}]),
            "page {pages}"
        );
        assert!(
            rows(&r).iter().all(|row| row[0].is_string()),
            "page {pages}"
        );
        // The rows are the ones of the one-shot read, at the right place in it.
        assert_eq!(rows(&r)[0], json!([seen.to_string(), seen]));
        seen += rows(&r).len();
        pages += 1;
        match r["next"]["cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!((pages, seen), (3, 100_500));
    assert_eq!(one_shot["columns"][0]["type"], "text");
    // A page asked for by offset alone, past the odd value, agrees too.
    let last =
        run(json!({"path": d.s(), "sql": "SELECT x, y FROM t OFFSET 100299 LIMIT 3"})).unwrap();
    assert_eq!(
        last["rows"],
        json!([["100299", 100299], ["abc", 100300], ["100301", 100301]])
    );
    assert_eq!(last["columns"][0]["type"], "text");
}
