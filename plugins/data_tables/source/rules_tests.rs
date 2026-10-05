//! The documented rules of a result: the order of an unordered shape, the types and limits of
//! integer sums, and the result of a shape that has no sortable column.
#![cfg(test)]
use parquet::basic::Compression;
use serde_json::{Value, json};

use crate::testkit::{Dir, fixtures, query};

fn q(d: &Dir, sql: &str) -> Value {
    query(d, sql).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn people() -> Dir {
    let d = Dir::new();
    d.put(
        "p.parquet",
        fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY),
    );
    d
}

#[test]
fn a_null_group_key_sorts_after_every_value_in_every_unordered_shape() {
    let d = Dir::new();
    d.put("t.csv", "g,n\nb,1\n,2\na,3\n,4\nc,5\n");
    d.put("u.csv", "x,y\n3,a\n,b\n1,c\n-7,d\n");
    for (sql, want) in [
        (
            "SELECT g, count(*) AS n FROM t GROUP BY g",
            json!([["a", 1], ["b", 1], ["c", 1], [null, 2]]),
        ),
        (
            "SELECT DISTINCT g FROM t",
            json!([["a"], ["b"], ["c"], [null]]),
        ),
        (
            "SELECT x, count(*) AS n FROM u GROUP BY x",
            json!([[-7, 1], [1, 1], [3, 1], [null, 1]]),
        ),
        (
            "SELECT g FROM t UNION SELECT g FROM t",
            json!([["a"], ["b"], ["c"], [null]]),
        ),
        (
            "SELECT g, n, row_number() OVER (PARTITION BY g ORDER BY n) AS rn FROM t",
            json!([
                ["a", 3, 1],
                ["b", 1, 1],
                ["c", 5, 1],
                [null, 2, 1],
                [null, 4, 2]
            ]),
        ),
    ] {
        let r = q(&d, sql);
        assert_eq!(r["order"], "columns", "{sql}");
        assert_eq!(r["rows"], want, "{sql}");
    }
}

#[test]
fn a_result_with_no_sortable_column_says_its_order_is_not_guaranteed() {
    let d = people();
    let note =
        "the result has no sortable column, so its row order is not guaranteed between calls";
    let r = q(&d, "SELECT DISTINCT tags FROM p");
    assert_eq!(r["order"], "none");
    assert_eq!(r["warnings"], json!([note]));
    let mut got: Vec<String> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::to_string)
        .collect();
    got.sort();
    assert_eq!(got, [r#"[[1,2]]"#, r#"[[]]"#, r#"[null]"#]);
    // One sortable column is enough to order by, and the list column simply rides along.
    let r = q(&d, "SELECT DISTINCT tags, id FROM p");
    assert_eq!(r["order"], "columns");
    assert_eq!(r["warnings"], json!([]));
    assert_eq!(r["rows"], json!([[[1, 2], 1], [[], 2], [null, 3]]));
    // A single row has no order to speak of, so it carries no warning.
    let r = q(&d, "SELECT DISTINCT tags FROM p LIMIT 1");
    assert_eq!(r["order"], "file");
    assert_eq!(r["warnings"], json!([]));
}

#[test]
fn an_unsigned_sum_is_unsigned_and_exact_up_to_the_largest_value() {
    let d = people();
    // `big` holds 18446744073709551615, 1 and NULL.
    let r = q(&d, "SELECT sum(big) AS s FROM p WHERE big < 100");
    assert_eq!(r["columns"], json!([{"name": "s", "type": "uint64"}]));
    assert_eq!(r["rows"], json!([[1]]));
    // Above the largest i64 and exactly the largest u64: exact, and a string like every
    // integer beyond 2^53.
    let r = q(&d, "SELECT sum(big) AS s FROM p WHERE id = 1");
    assert_eq!(r["columns"], json!([{"name": "s", "type": "uint64"}]));
    assert_eq!(r["rows"], json!([["18446744073709551615"]]));
    // One more than the largest u64 fails instead of wrapping to 0.
    let e = query(&d, "SELECT sum(big) AS s FROM p").unwrap_err();
    assert!(e.contains("out of range UInt64"), "{e}");
    // A signed column still sums as int64.
    let r = q(&d, "SELECT sum(id) AS s FROM p");
    assert_eq!(r["columns"], json!([{"name": "s", "type": "int64"}]));
    assert_eq!(r["rows"], json!([[6]]));
}

#[test]
fn unsigned_and_signed_integers_mix_exactly_near_the_edges() {
    let d = Dir::new();
    for (sql, want) in [
        (
            "SELECT CAST(9223372036854775807 AS BIGINT) + CAST(9223372036854775807 AS BIGINT UNSIGNED) AS v",
            json!("18446744073709551614"),
        ),
        (
            "SELECT CAST(9223372036854775807 AS BIGINT UNSIGNED) * CAST(2 AS BIGINT) AS v",
            json!("18446744073709551614"),
        ),
        (
            "SELECT CAST(-1 AS BIGINT) + CAST(1 AS BIGINT UNSIGNED) AS v",
            // A decimal result is always a string, whatever its size.
            json!("0"),
        ),
    ] {
        assert_eq!(q(&d, sql)["rows"], json!([[want]]), "{sql}");
    }
    for sql in [
        "SELECT CAST(1 AS BIGINT) + CAST(1 AS BIGINT UNSIGNED)",
        "SELECT CAST(1 AS BIGINT UNSIGNED) * CAST(1 AS BIGINT)",
        "SELECT CAST(18446744073709551615 AS BIGINT UNSIGNED) * CAST(2 AS BIGINT)",
    ] {
        match query(&d, sql) {
            Ok(_) => {}
            Err(e) => assert!(!e.contains("checked_") && !e.contains('\n'), "{sql}: {e}"),
        }
    }
}
