//! Limits: what one result may hold, what a call may build, and the exact edges of both. Every
//! number in an assertion is written out, so a limit that moves by one fails here.
#![cfg(test)]
use datafusion::common::DataFusionError;
use serde_json::{Value, json};

use crate::engine::explain;
use crate::query::{MAX_CELL_BYTES, OUTPUT_BUDGET};
use crate::testkit::{Dir, query, rows, run};

/// The bytes a result takes on the wire.
fn wire(v: &Value) -> usize {
    serde_json::to_string(v).unwrap().len()
}

fn err(d: &Dir, sql: &str) -> String {
    query(d, sql)
        .err()
        .unwrap_or_else(|| panic!("{sql} succeeded"))
}

/// A table of `rows` rows and ten text columns of 50 characters, which is 530 bytes of JSON a row.
fn wide_text(rows: usize) -> Dir {
    let d = Dir::new();
    let header: Vec<String> = (0..10).map(|c| format!("c{c}")).collect();
    let mut text = header.join(",") + "\n";
    for r in 0..rows {
        let cells: Vec<String> = (0..10).map(|c| format!("c{c}-{r:0>44}")).collect();
        text.push_str(&cells.join(","));
        text.push('\n');
    }
    d.put("t.csv", text);
    d
}

#[test]
fn a_page_of_three_megabytes_of_rows_and_a_thousand_markdown_rows_fits_the_output_limit() {
    // 3 MB of rows beside a Markdown table used to be 4.5 MB on the wire, past the host's limit.
    let d = wide_text(7_000);
    let host_limit = 4 * 1024 * 1024;
    assert!(OUTPUT_BUDGET < host_limit);
    let (mut seen, mut pages, mut cursor) = (0usize, 0usize, None::<String>);
    loop {
        let mut input = json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 100000, "max_bytes": 3000000, "markdown_rows": 1000});
        if let Some(c) = &cursor {
            input["cursor"] = json!(c);
        }
        let r = run(input).unwrap();
        assert!(
            wire(&r) <= OUTPUT_BUDGET,
            "page {pages}: {} bytes",
            wire(&r)
        );
        assert_eq!(r["offset"], seen);
        for (i, row) in rows(&r).iter().enumerate() {
            assert_eq!(row[0], format!("c0-{:0>44}", seen + i), "row {}", seen + i);
        }
        seen += rows(&r).len();
        pages += 1;
        match r["next"]["cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen, 7_000);
    // 530 bytes a row: the first page stops at the 3,000,000 byte bound (5,661 rows).
    assert_eq!(pages, 2);
}

#[test]
fn rows_are_cut_to_leave_room_for_wide_column_names_and_the_pages_still_join() {
    // 20 columns named with 40,000 characters: 0.8 MB of column list and 0.8 MB of Markdown
    // header on every page, beside 3 KB rows.
    let d = Dir::new();
    let names: Vec<String> = (0..20).map(|c| format!("{c:0>40000}")).collect();
    let mut text = names.join(",") + "\n";
    for r in 0..1_000 {
        let cells: Vec<String> = (0..20).map(|c| format!("{c}-{r:0>146}")).collect();
        text.push_str(&(cells.join(",") + "\n"));
    }
    d.put("t.csv", text);
    let (mut seen, mut pages, mut cut, mut cursor) = (0usize, 0usize, 0usize, None::<String>);
    loop {
        let mut input = json!({"path": d.s(), "sql": "SELECT * FROM t", "max_rows": 100000, "max_bytes": 3000000, "markdown_rows": 1000});
        if let Some(c) = &cursor {
            input["cursor"] = json!(c);
        }
        let r = run(input).unwrap();
        assert!(
            wire(&r) <= OUTPUT_BUDGET,
            "page {pages}: {} bytes",
            wire(&r)
        );
        for (i, row) in rows(&r).iter().enumerate() {
            assert_eq!(row[0], format!("0-{:0>146}", seen + i));
        }
        let n = rows(&r).len();
        seen += n;
        pages += 1;
        let said = r["warnings"].as_array().unwrap().iter().any(|w| {
            w.as_str().unwrap()
                == format!(
                    "the page was cut to {n} rows to fit the output limit; read on with next.cursor"
                )
        });
        match r["next"]["cursor"].as_str() {
            Some(c) => {
                assert!(said, "{:?}", r["warnings"]);
                cut += 1;
                cursor = Some(c.to_string());
            }
            None => {
                assert!(!said);
                break;
            }
        }
    }
    assert_eq!(seen, 1_000);
    assert!(pages >= 2 && cut == pages - 1, "{pages} pages, {cut} cut");
}

#[test]
fn the_markdown_table_has_its_own_byte_cap_and_says_how_many_rows_it_shows() {
    let d = wide_text(1_000);
    let r = run(
        json!({"path": d.s(), "sql": "SELECT * FROM t", "markdown_rows": 1000, "max_rows": 1000}),
    )
    .unwrap();
    let md = r["markdown"].as_str().unwrap();
    // 256 KiB of table: each row is about 520 characters, so about 504 rows are shown.
    assert!(md.len() < 256 * 1024 + 2000, "{}", md.len());
    let shown = md.lines().filter(|l| l.starts_with("| c0-")).count();
    assert!((400..=600).contains(&shown), "{shown}");
    assert!(
        md.ends_with(&format!(
            "(the first {shown} of 1000 rows of this page; all are in rows)"
        )),
        "{}",
        &md[md.len() - 80..]
    );
    assert_eq!(rows(&r).len(), 1000);
}

#[test]
fn a_cell_that_is_a_huge_list_is_cut_at_64_kib_without_being_built() {
    let d = Dir::new();
    let mut text = String::from("n\n");
    for i in 1..=300_000 {
        text.push_str(&format!("{i}\n"));
    }
    d.put("t.csv", text);
    let r = query(&d, "SELECT array_agg(n) AS a FROM t").unwrap();
    assert_eq!(r["columns"], json!([{"name": "a", "type": "list<int64>"}]));
    let cell = r["rows"][0][0].as_str().unwrap();
    assert_eq!(cell.len(), MAX_CELL_BYTES);
    assert!(
        cell.starts_with("[1,2,3,4,5,6,7,8,9,10,11,"),
        "{}",
        &cell[..40]
    );
    assert_eq!(
        r["warnings"],
        json!([
            "1 values were cut at 64 KiB; select a part of them (SUBSTR, a list slice or one field) to read the rest"
        ])
    );
    assert_eq!(r["order"], "file");
    assert!(wire(&r) < 200_000, "{}", wire(&r));
    // A record holding such a list is cut the same way, and a short one is left whole.
    let r = query(
        &d,
        "SELECT named_struct('a', array_agg(n), 'b', 1) AS s, array_agg(n) FILTER (WHERE n < 4) AS short FROM t",
    )
    .unwrap();
    assert!(r["rows"][0][0].is_string());
    assert_eq!(r["rows"][0][1], json!([1, 2, 3]));
}

#[test]
fn a_row_of_long_texts_is_shortened_evenly_and_still_fits() {
    let d = Dir::new();
    let header: Vec<String> = (0..100).map(|c| format!("c{c}")).collect();
    let cell = "x".repeat(60_000);
    let row = vec![cell.as_str(); 100].join(",");
    d.put("t.csv", format!("{}\n{row}\n", header.join(",")));
    let r = query(&d, "SELECT * FROM t").unwrap();
    assert!(wire(&r) <= OUTPUT_BUDGET, "{}", wire(&r));
    let cells = r["rows"][0].as_array().unwrap();
    assert_eq!(cells.len(), 100);
    // A 1 MiB row over 100 columns: 10485 bytes each.
    assert!(cells.iter().all(|c| c.as_str().unwrap().len() == 10_485));
    assert_eq!(
        r["warnings"],
        json!([
            "100 long text values were shortened so that a row fits in 1024 KiB; select fewer columns to read them whole"
        ])
    );
}

#[test]
fn a_result_that_cannot_fit_even_one_row_names_what_to_narrow() {
    let d = Dir::new();
    let names: Vec<String> = (0..1_500).map(|c| format!("{c:0>3000}")).collect();
    let row: Vec<String> = (0..1_500).map(|c| c.to_string()).collect();
    d.put("t.csv", format!("{}\n{}\n", names.join(","), row.join(",")));
    assert_eq!(
        err(&d, "SELECT * FROM t"),
        "the result is too large to return; select fewer columns, or cut long values with SUBSTR"
    );
}

#[test]
fn the_page_byte_budget_ends_a_page_exactly_at_the_bound() {
    // Rows `[100000]` are 8 bytes and a separator: 9 bytes each, so 456 rows are 4104 bytes.
    let d = Dir::new();
    let mut text = String::from("n\n");
    for i in 100_000..101_000 {
        text.push_str(&format!("{i}\n"));
    }
    d.put("t.csv", text);
    let page = |max_bytes: u64| {
        let r = run(json!({"path": d.s(), "sql": "SELECT n FROM t", "max_rows": 100000, "max_bytes": max_bytes})).unwrap();
        (rows(&r).len(), r["next"]["cursor"].is_string())
    };
    // At the bound a page is full: the next row would start over it.
    assert_eq!(page(4104), (456, true));
    // One byte under the bound still takes the 456th row, which crosses it.
    assert_eq!(page(4103), (456, true));
    // One byte over: the 456th row ends under the bound, so a 457th is taken.
    assert_eq!(page(4105), (457, true));
    assert_eq!(page(4096), (456, true));
    // A bound past the whole table returns all of it and no cursor.
    assert_eq!(page(30_000), (1000, false));
}

#[test]
fn a_text_cut_on_a_multibyte_character_backs_up_to_a_boundary() {
    let d = Dir::new();
    for (tail, kept) in [("€", 65_535), ("😀", 65_535), ("é", 65_535)] {
        // Byte 65536 falls inside the character that starts at byte 65535.
        let text = format!("{}{tail}tail", "a".repeat(65_535));
        d.put("t.csv", format!("v\n{text}\nshort\n"));
        let r = query(&d, "SELECT v FROM t").unwrap();
        let cell = r["rows"][0][0].as_str().unwrap();
        assert_eq!(cell.len(), kept, "{tail}");
        assert!(cell.bytes().all(|b| b == b'a'), "{tail}");
        assert_eq!(r["rows"][1][0], "short");
        assert_eq!(r["warnings"].as_array().unwrap().len(), 1, "{tail}");
    }
    // A character that starts before the cap and ends after it moves back by the whole character.
    d.put("t.csv", format!("v\n{}😀\n", "a".repeat(65_534)));
    let r = query(&d, "SELECT v FROM t").unwrap();
    assert_eq!(r["rows"][0][0].as_str().unwrap().len(), 65_534);
    // A character that ends exactly at the cap stays.
    d.put("t.csv", format!("v\n{}€x\n", "a".repeat(65_533)));
    let r = query(&d, "SELECT v FROM t").unwrap();
    let cell = r["rows"][0][0].as_str().unwrap();
    assert_eq!(cell.len(), 65_536);
    assert!(cell.ends_with('€'));
}

#[test]
fn functions_that_build_a_value_from_a_count_are_refused_when_the_count_is_too_big() {
    let d = Dir::new();
    for (sql, want) in [
        (
            "SELECT repeat('ab', 1000000000)",
            "repeat would build 2000000000 bytes, over the 67108864 this tool allows; use a smaller size",
        ),
        (
            "SELECT repeat('a', 1000 * 1000 * 1000)",
            "repeat would build 1000000000 bytes, over the 67108864",
        ),
        (
            "SELECT lpad('x', 2000000000, 'ab')",
            "lpad would build 2000000000 characters, over the 67108864",
        ),
        (
            "SELECT rpad('x', 2000000000, 'ab')",
            "rpad would build 2000000000 characters, over the 67108864",
        ),
        (
            "SELECT unnest(range(0, 400000000))",
            "range would build 400000000 elements, over the 20000000 this tool allows",
        ),
        (
            "SELECT count(*) FROM (SELECT unnest(generate_series(1, 400000000)) AS v)",
            "generate_series would build 399999999 elements, over the 20000000",
        ),
        (
            "SELECT range(0, 1000000000, 2)",
            "range would build 500000000 elements, over the 20000000",
        ),
        (
            "SELECT array_repeat(1, 100000000)",
            "array_repeat would build 100000000 elements, over the 20000000",
        ),
        (
            "SELECT range(20000001)",
            "range would build 20000001 elements, over the 20000000",
        ),
    ] {
        let e = err(&d, sql);
        assert!(e.starts_with("the query failed: "), "{sql}: {e}");
        assert!(e.contains(want), "{sql}: {e}");
        assert!(!e.contains('\n'), "{sql}: {e}");
    }
    // Sizes within the caps run, and the edge itself is allowed.
    for (sql, want) in [
        ("SELECT repeat('ab', 3)", json!("ababab")),
        ("SELECT lpad('x', 5, 'ab')", json!("ababx")),
        ("SELECT rpad('x', 5, 'ab')", json!("xabab")),
        ("SELECT cardinality(range(0, 20000000))", json!(20_000_000)),
        ("SELECT cardinality(array_repeat(1, 3))", json!(3)),
        ("SELECT length(repeat('a', 67108864))", json!(67_108_864)),
    ] {
        let r = query(&d, sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(r["rows"][0][0], want, "{sql}");
    }
}

#[test]
fn an_engine_allocation_failure_is_the_plain_narrow_it_sentence() {
    let narrow = "the query needs more memory than this tool may use; narrow it with WHERE or LIMIT, select fewer columns, or aggregate before joining";
    for text in [
        "Range too large to materialize: failed to allocate 3200000000 bytes",
        "Execution error: memory allocation failed because the computed capacity exceeded",
        "the computed capacity exceeded the collection's maximum",
        "capacity overflow",
    ] {
        assert_eq!(
            explain(&DataFusionError::Execution(text.into())),
            narrow,
            "{text}"
        );
    }
    assert_eq!(
        explain(&DataFusionError::ResourcesExhausted("pool".into())),
        narrow
    );
}

#[test]
fn a_query_that_reads_no_table_needs_no_path() {
    let r = run(json!({"sql": "SELECT 1 + 1 AS two, 'a' AS s"})).unwrap();
    assert_eq!(r["rows"], json!([[2, "a"]]));
    assert_eq!(
        r["columns"],
        json!([{"name": "two", "type": "int64"}, {"name": "s", "type": "text"}])
    );
    for input in [
        json!({}),
        json!({"mode": "tables"}),
        json!({"mode": "describe"}),
        json!({"mode": "export", "sql": "SELECT 1", "output": "o.csv"}),
    ] {
        assert_eq!(
            run(input.clone()).unwrap_err(),
            "give path (a data file or a folder of them) or tables (rows as JSON)",
            "{input}"
        );
    }
}
