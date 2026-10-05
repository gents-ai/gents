//! End-to-end tests of the three read modes through the whole plugin: exact
//! results, ordering rules, paging, refusals and limits.
#![cfg(test)]
use std::sync::Arc;

use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::csv::Options;
use crate::engine::{Engine, explain};
use crate::testkit::{Dir, fixtures, query, rows, run};
use parquet::basic::Compression;

const SALES: &str = "region,units,price,day\nnorth,10,2.5,2024-01-01\nsouth,7,3,2024-01-02\nnorth,5,4,2024-01-03\neast,,1.5,2024-01-04\nsouth,3,2,2024-01-05\n";

fn shop() -> Dir {
    let d = Dir::new();
    d.put("sales.csv", SALES);
    d.put("regions.csv", "region,boss\nnorth,Ann\nsouth,Bo\nwest,Cy\n");
    d.put("people.csv", "name,score,joined,active\nAna,9,2024-01-05,true\n\"Bo, Jr.\",7,2024-02-10,false\nCy,,2024-03-01,true\n");
    d.put("Report 1.csv", "Order Date,Total\n2024-01-01,5\n");
    d
}

fn q(d: &Dir, sql: &str) -> Value {
    query(d, sql).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn err(d: &Dir, sql: &str) -> String {
    query(d, sql)
        .err()
        .unwrap_or_else(|| panic!("{sql} succeeded"))
}

#[test]
fn select_star_gives_exact_columns_rows_and_markdown() {
    let d = shop();
    let r = q(&d, "SELECT * FROM people");
    assert_eq!(
        r,
        json!({
            "columns": [{"name": "name", "type": "text"}, {"name": "score", "type": "int64"}, {"name": "joined", "type": "date"}, {"name": "active", "type": "bool"}],
            "rows": [["Ana", 9, "2024-01-05", true], ["Bo, Jr.", 7, "2024-02-10", false], ["Cy", null, "2024-03-01", true]],
            "row_count": 3,
            "offset": 0,
            "order": "file",
            "markdown": "| name | score | joined | active |\n| --- | --- | --- | --- |\n| Ana | 9 | 2024-01-05 | true |\n| Bo, Jr. | 7 | 2024-02-10 | false |\n| Cy | NULL | 2024-03-01 | true |",
            "warnings": []
        })
    );
}

#[test]
fn a_scan_keeps_file_order_and_a_grouped_result_is_sorted_by_its_columns() {
    let d = shop();
    let r = q(
        &d,
        "SELECT region, sum(units) AS u, count(*) AS n FROM sales GROUP BY region",
    );
    assert_eq!(
        r["rows"],
        json!([["east", null, 1], ["north", 15, 2], ["south", 10, 2]])
    );
    assert_eq!(r["order"], "columns");
    assert_eq!(
        r["columns"],
        json!([{"name": "region", "type": "text"}, {"name": "u", "type": "int64"}, {"name": "n", "type": "int64"}])
    );
    let r = q(&d, "SELECT * FROM sales WHERE units > 5");
    assert_eq!(
        r["rows"],
        json!([
            ["north", 10, 2.5, "2024-01-01"],
            ["south", 7, 3.0, "2024-01-02"]
        ])
    );
    assert_eq!(r["order"], "file");
    let r = q(
        &d,
        "SELECT region FROM sales ORDER BY units DESC NULLS LAST, region LIMIT 3",
    );
    assert_eq!(r["rows"], json!([["north"], ["south"], ["north"]]));
    assert_eq!(r["order"], "query");
}

#[test]
fn joins_windows_and_distinct_are_sorted_by_their_columns_too() {
    let d = shop();
    let r = q(
        &d,
        "SELECT s.region, r.boss, s.units FROM sales s JOIN regions r ON s.region = r.region",
    );
    assert_eq!(
        r["rows"],
        json!([
            ["north", "Ann", 5],
            ["north", "Ann", 10],
            ["south", "Bo", 3],
            ["south", "Bo", 7]
        ])
    );
    assert_eq!(r["order"], "columns");
    let r = q(
        &d,
        "SELECT region, units, row_number() OVER (PARTITION BY region ORDER BY day) AS rn FROM sales",
    );
    assert_eq!(
        r["rows"],
        json!([
            ["east", null, 1],
            ["north", 5, 2],
            ["north", 10, 1],
            ["south", 3, 2],
            ["south", 7, 1]
        ])
    );
    let r = q(&d, "SELECT DISTINCT region FROM sales");
    assert_eq!(r["rows"], json!([["east"], ["north"], ["south"]]));
    let r = q(
        &d,
        "SELECT region FROM sales UNION SELECT region FROM regions",
    );
    assert_eq!(r["rows"], json!([["east"], ["north"], ["south"], ["west"]]));
}

#[test]
fn ctes_subqueries_functions_and_aggregates() {
    let d = shop();
    assert_eq!(
        q(
            &d,
            "WITH t AS (SELECT region, units FROM sales WHERE units IS NOT NULL) SELECT count(*) AS c FROM t"
        )["rows"],
        json!([[4]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT upper(region), substr(region, 1, 2), char_length(region), extract(month FROM day) FROM sales WHERE region = 'east'"
        )["rows"],
        json!([["EAST", "ea", 4, 1]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT CASE WHEN units >= 7 THEN 'big' ELSE 'small' END AS b, CAST(price AS INT) AS p FROM sales WHERE region = 'north'"
        )["rows"],
        json!([["big", 2], ["small", 4]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT avg(price) AS a, min(day) AS mn, max(day) AS mx, count(units) AS cu, count(DISTINCT region) AS dr FROM sales"
        )["rows"],
        json!([[2.6, "2024-01-01", "2024-01-05", 4, 3]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT region FROM sales WHERE region LIKE 'n%' AND units IN (5, 10) ORDER BY units"
        )["rows"],
        json!([["north"], ["north"]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT region, (SELECT max(units) FROM sales) AS m FROM regions WHERE region = 'west'"
        )["rows"],
        json!([["west", 10]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT regexp_like(boss, '^A') AS r FROM regions ORDER BY boss"
        )["rows"],
        json!([[true], [false], [false]])
    );
}

#[test]
fn identifiers_keep_their_case_and_quotes_reach_names_with_spaces() {
    let d = shop();
    let r = q(&d, "SELECT \"Order Date\", Total FROM Report_1");
    assert_eq!(r["rows"], json!([["2024-01-01", 5]]));
    assert_eq!(
        r["columns"],
        json!([{"name": "Order Date", "type": "date"}, {"name": "Total", "type": "int64"}])
    );
    assert!(err(&d, "SELECT total FROM Report_1").contains("total"));
}

#[test]
fn literals_nulls_and_empty_strings_stay_distinct() {
    let d = shop();
    let r = q(&d, "SELECT 'x' AS a, '' AS b, NULL AS c");
    assert_eq!(r["rows"], json!([["x", "", null]]));
    assert_eq!(r["columns"][2], json!({"name": "c", "type": "null"}));
    assert_eq!(
        r["markdown"],
        "| a | b | c |\n| --- | --- | --- |\n| x |  | NULL |"
    );
}

#[test]
fn nan_and_the_infinities_are_strings_and_the_result_says_so() {
    let d = shop();
    let r = q(
        &d,
        "SELECT CAST('NaN' AS DOUBLE) AS a, 1.0 / 0.0 AS b, -1.0 / 0.0 AS c, 0.1 AS d, 1e300 * 10 AS e",
    );
    assert_eq!(
        r["rows"],
        json!([["NaN", "Infinity", "-Infinity", 0.1, 1e301]])
    );
    assert_eq!(
        r["warnings"],
        json!([
            "3 float values are NaN or infinite and appear as the strings \"NaN\", \"Infinity\" and \"-Infinity\""
        ])
    );
}

#[test]
fn parquet_values_keep_exact_types() {
    let d = Dir::new();
    d.put(
        "people.parquet",
        fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY),
    );
    let r = q(
        &d,
        "SELECT id, big, price, score, born, seen, nested, tags FROM people",
    );
    assert_eq!(
        r["rows"],
        json!([
            [1, "18446744073709551615", "123.45", 1.5, "2024-02-29", "2024-02-29T12:30:45.500", {"x": 1, "y": "a"}, [1, 2]],
            [2, 1, null, null, null, null, {"x": null, "y": "b"}, []],
            [3, null, "-0.05", "NaN", "1970-01-01", "1970-01-01T00:00:00", {"x": 3, "y": null}, null]
        ])
    );
    let types: Vec<&str> = r["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "int64",
            "uint64",
            "decimal(10,2)",
            "float64",
            "date",
            "timestamp",
            "struct",
            "list<int32>"
        ]
    );
    assert_eq!(
        r["warnings"],
        json!([
            "1 float values are NaN or infinite and appear as the strings \"NaN\", \"Infinity\" and \"-Infinity\"",
            "1 integers beyond 2^53 appear as strings so no digit is lost"
        ])
    );
    // Decimals compare and add exactly; struct fields and list items are reachable.
    assert_eq!(
        q(
            &d,
            "SELECT price + CAST(0.01 AS DECIMAL(10, 2)) AS p, nested['x'] AS x, tags[1] AS t FROM people WHERE id = 1"
        )["rows"],
        json!([["123.46", 1, 1]])
    );
}

#[test]
fn a_projection_in_another_order_than_the_file_reads_the_right_columns() {
    let d = Dir::new();
    d.put(
        "p.parquet",
        fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY),
    );
    d.put("s.csv", SALES);
    assert_eq!(
        q(&d, "SELECT name, id FROM p WHERE id < 3")["rows"],
        json!([["Ana", 1], ["", 2]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT price, region, price AS again FROM s WHERE region = 'east'"
        )["rows"],
        json!([[1.5, "east", 1.5]])
    );
    assert_eq!(q(&d, "SELECT count(*) AS n FROM p")["rows"], json!([[3]]));
}

#[test]
fn only_select_queries_are_accepted_and_nothing_is_written() {
    let d = shop();
    let target = std::env::temp_dir().join(format!("dt-never-{}", std::process::id()));
    let guard = format!("COPY (SELECT 1) TO '{}'", target.display());
    for sql in [
        "CREATE TABLE x AS SELECT 1",
        "DROP TABLE people",
        "INSERT INTO people VALUES ('a', 1, DATE '2024-01-01', true)",
        guard.as_str(),
        "CREATE EXTERNAL TABLE e STORED AS CSV LOCATION '/etc'",
        "SET datafusion.execution.batch_size = 1",
        "UPDATE people SET score = 1",
        "DELETE FROM people",
        "CREATE VIEW v AS SELECT 1",
        "ALTER TABLE people ADD COLUMN x INT",
        "DESCRIBE people",
        "SHOW TABLES",
        "TRUNCATE TABLE people",
        "CREATE SCHEMA s",
        "VACUUM",
    ] {
        assert_eq!(
            err(&d, sql),
            "only SELECT queries are accepted; to save a result as a file use the export mode",
            "{sql}"
        );
    }
    assert!(!target.exists());
    assert!(err(&d, "SELECT 1 INTO t").contains("the query failed"));
    assert_eq!(
        err(&d, "SELECT 1; SELECT 2"),
        "the query failed: This feature is not implemented: The context currently only supports a single SQL statement"
    );
    assert_eq!(
        q(&d, "EXPLAIN SELECT 1")["columns"],
        json!([{"name": "plan_type", "type": "text"}, {"name": "plan", "type": "text"}])
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("people.csv"))
            .unwrap()
            .lines()
            .count(),
        4
    );
}

#[test]
fn sql_cannot_reach_files_outside_the_bound_folder() {
    let outside = Dir::new();
    outside.put("secret.csv", "k\ntop\n");
    let d = shop();
    for sql in [
        format!(
            "SELECT * FROM '{}'",
            outside.path().join("secret.csv").display()
        ),
        format!(
            "SELECT * FROM read_csv('{}')",
            outside.path().join("secret.csv").display()
        ),
        format!(
            "CREATE EXTERNAL TABLE s STORED AS CSV LOCATION '{}'",
            outside.path().display()
        ),
        "SELECT * FROM '/etc/passwd'".to_string(),
        "SELECT * FROM file:///etc/passwd".to_string(),
        "SELECT * FROM information_schema.tables".to_string(),
    ] {
        let e = err(&d, &sql);
        assert!(
            e.starts_with("the query failed:") || e.starts_with("only SELECT"),
            "{sql}: {e}"
        );
        assert!(!e.contains("top"), "{e}");
    }
}

#[test]
fn engine_errors_are_one_plain_sentence() {
    let d = shop();
    for (sql, want) in [
        ("SELEC 1", "the query failed: "),
        ("SELECT nope FROM sales", "nope"),
        (
            "SELECT * FROM nope",
            "the query failed: table 'nope' not found",
        ),
        (
            "SELECT 1 / 0",
            "the query failed: Divide by zero error (guard the divisor with NULLIF)",
        ),
        (
            "SELECT units % 0 FROM sales",
            "Divide by zero error (guard the divisor with NULLIF)",
        ),
        (
            "SELECT 9223372036854775807 + 1",
            "(cast the values to a wider type, such as DECIMAL(38,0), first)",
        ),
        (
            "SELECT 9223372036854775807 * 2",
            "Overflow happened on: 9223372036854775807 * 2",
        ),
        ("SELECT -9223372036854775807 - 2", "Overflow happened on"),
        (
            "SELECT sum(x) FROM (VALUES (9223372036854775807), (1)) AS t(x)",
            "out of range Int64",
        ),
        ("SELECT CAST('abc' AS INT)", "Cannot cast string 'abc'"),
        ("SELECT CAST(9.3e18 AS BIGINT)", "Can't cast value"),
        ("SELECT abs(-9223372036854775807 - 1)", "overflow"),
        ("SELECT sqrt(-1)", "square root of a negative"),
    ] {
        let e = err(&d, sql);
        assert!(e.contains(want), "{sql}: {e}");
        assert!(
            !e.contains('\n') && !e.contains("datafusion.public"),
            "{sql}: {e}"
        );
    }
}

#[test]
fn integer_arithmetic_that_fits_is_exact_and_the_name_reads_like_the_expression() {
    let d = shop();
    let r = q(
        &d,
        "SELECT units + 1 AS a, units * 2 AS b, units - 20 AS c FROM sales WHERE region = 'north' ORDER BY units",
    );
    assert_eq!(r["rows"], json!([[6, 10, -15], [11, 20, -10]]));
    let r = q(
        &d,
        "SELECT units + 1, 2 * 3, sum(units) FROM sales GROUP BY units HAVING units > 9",
    );
    assert_eq!(
        r["columns"],
        json!([{"name": "sales.units + Int64(1)", "type": "int64"}, {"name": "Int64(2) * Int64(3)", "type": "int64"}, {"name": "sum(sales.units)", "type": "int64"}])
    );
    assert_eq!(r["rows"], json!([[11, 6, 10]]));
    assert_eq!(
        q(
            &d,
            "SELECT 9223372036854775806 + 1 AS m, -9223372036854775807 - 1 AS n"
        )["rows"],
        json!([["9223372036854775807", "-9223372036854775808"]])
    );
    assert_eq!(
        q(
            &d,
            "SELECT sum(x) AS s FROM (VALUES (9223372036854775806), (1)) AS t(x)"
        )["rows"],
        json!([["9223372036854775807"]])
    );
    assert_eq!(
        q(&d, "SELECT 1.5 + 2 AS f, 7 / 2 AS i, 7 % 4 AS m")["rows"],
        json!([[3.5, 3, 3]])
    );
}

#[test]
fn sql_that_is_too_long_or_too_deep_is_refused_not_run() {
    let d = shop();
    let long = format!("SELECT '{}'", "x".repeat(70_000));
    assert_eq!(
        err(&d, &long),
        "the SQL is over 64 KiB; shorten it or move literal data into a table"
    );
    let deep = format!("SELECT {}", vec!["1"; 300].join("+"));
    assert_eq!(
        err(&d, &deep),
        "an expression nests more than 256 levels deep; split it, or use IN (...) for a long list of OR conditions"
    );
    let nested = format!("SELECT {}1{}", "(".repeat(2000), ")".repeat(2000));
    assert_eq!(
        err(&d, &nested),
        "an expression nests more than 256 levels deep; split it, or use IN (...) for a long list of OR conditions"
    );
    let fine = format!("SELECT {} AS n", vec!["1"; 120].join("+"));
    assert_eq!(q(&d, &fine)["rows"], json!([[120]]));
    let in_list: Vec<String> = (0..5000).map(|i| i.to_string()).collect();
    assert_eq!(
        q(
            &d,
            &format!(
                "SELECT count(*) AS c FROM sales WHERE units IN ({})",
                in_list.join(",")
            )
        )["rows"],
        json!([[4]])
    );
}

#[test]
fn a_query_that_does_not_fit_in_memory_is_refused_with_what_to_narrow() {
    let d = Dir::new();
    let mut text = String::from("id,pad\n");
    for i in 0..100_000 {
        text.push_str(&format!("{i},{}\n", "x".repeat(60)));
    }
    d.put("big.csv", text);
    let catalog =
        Arc::new(Catalog::discover(Some(&d.s()), None, None, Options::default()).unwrap());
    let engine = Engine::new(catalog, 1024 * 1024).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let message = rt.block_on(async {
        let prepared = engine
            .prepare("SELECT * FROM big ORDER BY pad DESC, id")
            .await
            .unwrap();
        match engine.stream(prepared).await {
            Err(e) => explain(&e),
            Ok(mut stream) => {
                use futures::StreamExt;
                loop {
                    match stream.next().await {
                        Some(Err(e)) => break explain(&e),
                        Some(Ok(_)) => {}
                        None => panic!("the sort fit in 1 MiB"),
                    }
                }
            }
        }
    });
    assert_eq!(
        message,
        "the query needs more memory than this tool may use; narrow it with WHERE or LIMIT, select fewer columns, or aggregate before joining"
    );
    // The same file scans with flat memory when nothing has to be held.
    assert_eq!(
        q(&d, "SELECT count(*) AS n, max(id) AS m FROM big")["rows"],
        json!([[100_000, 99_999]])
    );
}

#[test]
fn types_inferred_from_a_sample_are_widened_by_a_second_pass_when_a_late_value_disagrees() {
    let d = Dir::new();
    let mut text = String::from("v\n");
    for i in 0..100_005 {
        text.push_str(&format!("{i}\n"));
    }
    text.push_str("late text\n");
    d.put("t.csv", text);
    assert_eq!(
        q(&d, "SELECT count(*) AS n FROM t")["rows"],
        json!([[100_006]])
    );
    let r = q(&d, "SELECT v FROM t ORDER BY v DESC LIMIT 2");
    assert_eq!(r["rows"], json!([["late text"], ["99999"]]));
    assert_eq!(r["columns"][0]["type"], "text");
    let r = q(&d, "SELECT count(*) AS n FROM t WHERE v = '7'");
    assert_eq!(r["rows"], json!([[1]]));
}

#[test]
fn markdown_is_bounded_and_says_so() {
    let d = Dir::new();
    d.put(
        "t.csv",
        format!(
            "n\n{}",
            (1..=60).map(|i| format!("{i}\n")).collect::<String>()
        ),
    );
    let r = run(json!({"path": d.s(), "sql": "SELECT n FROM t", "markdown_rows": 3})).unwrap();
    assert_eq!(r["row_count"], 60);
    assert_eq!(
        r["markdown"],
        "| n |\n| --- |\n| 1 |\n| 2 |\n| 3 |\n\n(the first 3 of 60 rows of this page; all are in rows)"
    );
    let r = run(json!({"path": d.s(), "sql": "SELECT n FROM t", "markdown_rows": 0})).unwrap();
    assert_eq!(
        r["markdown"],
        "| n |\n| --- |\n\n(the first 0 of 60 rows of this page; all are in rows)"
    );
    assert_eq!(rows(&r).len(), 60);
}

#[test]
fn long_text_cells_are_cut_and_said() {
    let d = Dir::new();
    d.put("t.csv", format!("a,b\n1,{}\n2,short\n", "é".repeat(70_000)));
    let r = q(&d, "SELECT b FROM t");
    let first = r["rows"][0][0].as_str().unwrap();
    assert!(
        first.len() <= crate::query::MAX_CELL_BYTES
            && first.len() > crate::query::MAX_CELL_BYTES - 4
    );
    assert!(first.chars().all(|c| c == 'é'));
    assert_eq!(r["rows"][1][0], "short");
    assert_eq!(
        r["warnings"],
        json!(["1 text values were cut at 64 KiB; select SUBSTR ranges to read the rest"])
    );
}

#[test]
fn inline_tables_are_queried_like_files_and_a_result_feeds_the_next_call() {
    let first = run(json!({"tables": [{"name": "t", "rows": [{"a": 1, "g": "x"}, {"a": 2, "g": "x"}, {"a": 4, "g": "y"}]}], "sql": "SELECT g, sum(a) AS s FROM t GROUP BY g"})).unwrap();
    assert_eq!(first["rows"], json!([["x", 3], ["y", 4]]));
    let second = run(json!({"tables": {"r": {"columns": first["columns"], "rows": first["rows"]}}, "sql": "SELECT max(s) AS m FROM r"})).unwrap();
    assert_eq!(second["rows"], json!([[4]]));
    let folder = shop();
    let both = run(json!({"path": folder.s(), "tables": {"extra": [[1]]}, "sql": "SELECT count(*) AS n FROM extra"})).unwrap();
    assert_eq!(both["rows"], json!([[1]]));
}

#[test]
fn tables_lists_columns_and_row_counts_that_are_known() {
    let d = shop();
    let r = run(json!({"path": d.s(), "mode": "tables"})).unwrap();
    let names: Vec<&str> = r["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Report_1", "people", "regions", "sales"]);
    assert_eq!(
        r["tables"][3],
        json!({"name": "sales", "source": "sales.csv", "format": "csv", "row_count": 5, "column_count": 4, "details": {"delimiter": ",", "header": true},
               "columns": [{"name": "region", "type": "text"}, {"name": "units", "type": "int64"}, {"name": "price", "type": "float64"}, {"name": "day", "type": "date"}]})
    );
    assert_eq!(r["warnings"], json!(["Report 1.csv is the table Report_1"]));
    let listed = run(json!({"path": d.s(), "mode": "tables", "table": "sales"})).unwrap();
    assert_eq!(listed["warnings"], json!([]));
    assert!(
        r["markdown"]
            .as_str()
            .unwrap()
            .contains("| sales | sales.csv | 5 | region, units, price, day |")
    );
    assert!(r.get("next").is_none());
    // A file that is only sampled has no row count rather than a guess.
    let big = Dir::new();
    big.put(
        "t.csv",
        format!(
            "n\n{}",
            (0..100_001).map(|i| format!("{i}\n")).collect::<String>()
        ),
    );
    let r = run(json!({"path": big.s()})).unwrap();
    assert_eq!(r["tables"][0]["row_count"], Value::Null);
    assert!(
        r["markdown"]
            .as_str()
            .unwrap()
            .contains("| t | t.csv | unknown | n |")
    );
    let r = run(json!({"path": d.s(), "table": "people"})).unwrap();
    assert_eq!(r["tables"].as_array().unwrap().len(), 1);
    let e = run(json!({"path": d.s(), "table": "nope"})).unwrap_err();
    assert_eq!(
        e,
        "there is no table named nope; the tables are Report_1, people, regions, sales"
    );
}

#[test]
fn a_table_that_cannot_be_read_is_listed_with_its_error() {
    let d = Dir::new();
    d.put("good.csv", "a\n1\n");
    d.put("bad.parquet", "PAR1 broken");
    let r = run(json!({"path": d.s()})).unwrap();
    assert_eq!(r["tables"][0]["name"], "bad");
    assert!(
        r["tables"][0]["error"]
            .as_str()
            .unwrap()
            .contains("is not a readable Parquet file")
    );
    assert_eq!(r["tables"][1]["name"], "good");
    assert!(
        r["markdown"]
            .as_str()
            .unwrap()
            .contains("| good | good.csv | 1 | a |")
    );
}

#[test]
fn tables_pages_through_a_large_folder_with_a_cursor() {
    let d = Dir::new();
    for i in 0..120 {
        d.put(&format!("f{i:03}.csv"), "x\n1\n");
    }
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut input = json!({"path": d.s()});
        if let Some(c) = &cursor {
            input["cursor"] = json!(c);
        }
        let r = run(input).unwrap();
        pages += 1;
        seen.extend(
            r["tables"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string()),
        );
        match r["next"]["cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(
        seen,
        (0..120).map(|i| format!("f{i:03}")).collect::<Vec<_>>()
    );
    let bad = run(json!({"path": d.s(), "cursor": "dt1.AAAA.0000000000000000"})).unwrap_err();
    assert!(bad.contains("cursor is not valid"), "{bad}");
    let r = run(json!({"path": d.s(), "max_rows": 7})).unwrap();
    assert_eq!(r["tables"].as_array().unwrap().len(), 7);
}

#[test]
fn describe_reports_exact_statistics_a_sample_and_when_it_only_saw_a_sample() {
    let d = shop();
    let r = run(json!({"path": d.s(), "mode": "describe", "table": "sales"})).unwrap();
    let t = &r["tables"][0];
    assert_eq!(t["row_count"], 5);
    assert_eq!(t["rows_scanned"], 5);
    assert_eq!(t["sampled"], false);
    assert_eq!(
        t["columns"],
        json!([
            {"name": "region", "type": "text", "non_null": 5, "nulls": 0, "distinct": 3, "min": "east", "max": "south"},
            {"name": "units", "type": "int64", "non_null": 4, "nulls": 1, "distinct": 4, "min": 3, "max": 10, "mean": 6.25},
            {"name": "price", "type": "float64", "non_null": 5, "nulls": 0, "distinct": 5, "min": 1.5, "max": 4.0, "mean": 2.6},
            {"name": "day", "type": "date", "non_null": 5, "nulls": 0, "distinct": 5, "min": "2024-01-01", "max": "2024-01-05"}
        ])
    );
    assert_eq!(
        t["sample"]["columns"],
        json!(["region", "units", "price", "day"])
    );
    assert_eq!(t["sample"]["rows"].as_array().unwrap().len(), 5);
    assert_eq!(
        t["sample"]["rows"][3],
        json!(["east", null, 1.5, "2024-01-04"])
    );
    assert!(r["markdown"].as_str().unwrap().starts_with(
        "### sales (5 rows)\n\n| column | type | non_null | distinct | min | max | mean |"
    ));

    let big = Dir::new();
    big.put(
        "t.csv",
        format!(
            "n\n{}",
            (0..5000).map(|i| format!("{i}\n")).collect::<String>()
        ),
    );
    let r = run(json!({"path": big.s(), "mode": "describe", "sample_rows": 1000})).unwrap();
    let t = &r["tables"][0];
    assert_eq!(
        (
            t["rows_scanned"].clone(),
            t["sampled"].clone(),
            t["row_count"].clone()
        ),
        (json!(1000), json!(true), json!(5000))
    );
    assert_eq!(t["columns"][0]["max"], 999);
    assert!(
        r["markdown"]
            .as_str()
            .unwrap()
            .starts_with("### t (5000 rows, statistics over the first 1000 rows)")
    );
}

#[test]
fn describe_handles_every_type_and_a_column_filter() {
    let d = Dir::new();
    d.put(
        "p.parquet",
        fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY),
    );
    let r = run(json!({"path": d.s(), "mode": "describe"})).unwrap();
    let cols = &r["tables"][0]["columns"];
    assert_eq!(
        cols[0],
        json!({"name": "id", "type": "int64", "non_null": 3, "nulls": 0, "distinct": 3, "min": 1, "max": 3, "mean": 2.0})
    );
    assert_eq!(
        cols[1],
        json!({"name": "name", "type": "text", "non_null": 2, "nulls": 1, "distinct": 2, "min": "", "max": "Ana"})
    );
    assert_eq!(cols[2]["non_null"], 2);
    assert_eq!(cols[2]["min"], 1.5);
    assert_eq!(
        cols[3],
        json!({"name": "active", "type": "bool", "non_null": 2, "nulls": 1, "distinct": 2})
    );
    assert_eq!(
        cols[8],
        json!({"name": "nested", "type": "struct", "non_null": 3, "nulls": 0})
    );
    assert_eq!(
        cols[9],
        json!({"name": "tags", "type": "list<int32>", "non_null": 2, "nulls": 1})
    );
    let r = run(json!({"path": d.s(), "mode": "describe", "columns": ["name", "id"]})).unwrap();
    let names: Vec<&str> = r["tables"][0]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["name", "id"]);
    assert_eq!(r["tables"][0]["columns_omitted"], 8);
    let e = run(json!({"path": d.s(), "mode": "describe", "columns": ["nope"]})).unwrap();
    assert_eq!(e["tables"][0]["error"], "table p has no column nope");
}

#[test]
fn describe_of_a_very_wide_table_names_the_columns_it_left_out() {
    let d = Dir::new();
    let header: Vec<String> = (0..300).map(|i| format!("c{i}")).collect();
    let row: Vec<String> = (0..300).map(|i| i.to_string()).collect();
    d.put(
        "w.csv",
        format!("{}\n{}\n", header.join(","), row.join(",")),
    );
    let r = run(json!({"path": d.s(), "mode": "describe"})).unwrap();
    assert_eq!(r["tables"][0]["columns"].as_array().unwrap().len(), 200);
    assert_eq!(r["tables"][0]["columns_omitted"], 100);
    assert!(
        r["warnings"][0]
            .as_str()
            .unwrap()
            .contains("described by its first 200 of 300 columns")
    );
    let r = run(json!({"path": d.s(), "mode": "tables"})).unwrap();
    assert_eq!(r["tables"][0]["column_count"], 300);
    assert_eq!(r["tables"][0]["columns"].as_array().unwrap().len(), 300);
}
