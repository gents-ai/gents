//! Hostile input: corrupt, truncated, mislabeled, empty and oversized files,
//! and a seeded mutation test that damages every kind of fixture many times.
//! Every outcome is a result or one plain sentence, never a panic or a hang.
#![cfg(test)]
use std::time::{Duration, Instant};

use parquet::basic::Compression;
use serde_json::{Value, json};

use crate::testkit::fixtures::{self, O, X};
use crate::testkit::{Dir, query, run};

/// A small deterministic generator (xorshift64*), so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn damage(bytes: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = bytes.to_vec();
    if b.is_empty() {
        return vec![rng.next() as u8];
    }
    match rng.below(6) {
        0 => {
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
        }
        1 => b.truncate(rng.below(b.len())),
        2 => {
            let at = rng.below(b.len());
            let junk: Vec<u8> = (0..1 + rng.below(16)).map(|_| rng.next() as u8).collect();
            b.splice(at..at, junk);
        }
        3 => {
            let (from, len) = (rng.below(b.len()), 1 + rng.below(32));
            let to = (from + len).min(b.len());
            b[from..to].fill(if rng.below(2) == 0 { 0 } else { 0xFF });
        }
        4 => {
            let (from, len) = (rng.below(b.len()), 1 + rng.below(64));
            let chunk: Vec<u8> = b[from..(from + len).min(b.len())].to_vec();
            let at = rng.below(b.len());
            b.splice(at..at, chunk);
        }
        _ => {
            let n = b.len();
            b.rotate_left(rng.below(n));
        }
    }
    b
}

fn fixtures_by_kind() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let batch = fixtures::sample_batch();
    vec![
        ("csv", "people.csv", b"name,score,joined,active\nAna,9,2024-01-05,true\n\"Bo, Jr.\",7,2024-02-10,false\nCy,,2024-03-01,true\n".to_vec()),
        ("tsv", "tabs.tsv", b"id\tnote\n1\tfirst\n2\t\"two\nlines\"\n".to_vec()),
        ("bom", "bom.csv", "\u{feff}k,v\n1,one\n2,two\n".as_bytes().to_vec()),
        ("utf16", "w.csv", { let mut b = vec![0xFF, 0xFE]; for u in "k,v\n1,é\n2,😀\n".encode_utf16() { b.extend_from_slice(&u.to_le_bytes()); } b }),
        ("json", "records.json", b"[{\"id\":1,\"name\":\"Ana\",\"tags\":[\"x\"],\"geo\":{\"lat\":1.5}},{\"id\":2,\"name\":null},{\"id\":3,\"extra\":true}]".to_vec()),
        ("ndjson", "events.ndjson", b"{\"t\":1,\"kind\":\"a\"}\n{\"t\":2,\"kind\":\"b\"}\n".to_vec()),
        ("parquet", "p.parquet", fixtures::parquet(&batch, Compression::SNAPPY)),
        ("parquet-zstd", "z.parquet", fixtures::parquet(&batch, Compression::ZSTD(Default::default()))),
        ("parquet-gzip", "g.parquet", fixtures::parquet(&batch, Compression::GZIP(Default::default()))),
        (
            "xlsx",
            "b.xlsx",
            fixtures::xlsx(&[("S", vec![vec![X::S("a"), X::S("b"), X::S("c")], vec![X::N("1"), X::D("45292"), X::B(true)], vec![X::I("x"), X::T("45294.5"), X::E("#N/A")]])]),
        ),
        ("ods", "b.ods", fixtures::ods(&[("S", vec![(1, vec![O::S("a"), O::S("b")]), (2, vec![O::F("1"), O::D("2024-01-31")])])])),
    ]
}

/// What a damaged file may do: succeed, or fail with one plain sentence.
fn check(outcome: Result<Value, String>, what: &str) {
    if let Err(e) = outcome {
        assert!(
            !e.is_empty() && !e.contains('\n') && e.len() < 600,
            "{what}: {e}"
        );
    }
}

#[test]
fn damaged_files_of_every_kind_never_panic_or_hang() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut failures = Vec::new();
    for (kind, name, bytes) in fixtures_by_kind() {
        for round in 0..90 {
            let damaged = damage(&bytes, &mut rng);
            let d = Dir::new();
            d.put(name, &damaged);
            if std::env::var("DT_TRACE").is_ok() {
                eprintln!("{kind} round {round}: {}", d.path().join(name).display());
            }
            let started = Instant::now();
            let table = name.rsplit_once('.').map_or(name, |(s, _)| s).to_string();
            let attempt = std::panic::catch_unwind(|| {
                let tables = run(json!({"path": d.s(), "mode": "tables"}));
                check(tables.clone(), "tables");
                if let Ok(t) = &tables {
                    for entry in t["tables"].as_array().into_iter().flatten() {
                        assert!(
                            entry.get("error").is_none()
                                || entry["error"].as_str().is_some_and(|e| !e.contains('\n'))
                        );
                    }
                }
                check(
                    run(
                        json!({"path": d.s(), "sql": format!("SELECT * FROM {table} LIMIT 20"), "max_rows": 50}),
                    ),
                    "select",
                );
                check(
                    run(
                        json!({"path": d.s(), "sql": format!("SELECT count(*) AS n FROM {table}")}),
                    ),
                    "count",
                );
                check(
                    run(json!({"path": d.s(), "mode": "describe", "sample_rows": 100})),
                    "describe",
                );
            });
            if attempt.is_err() {
                failures.push(format!(
                    "{kind} round {round} ({} bytes) panicked",
                    damaged.len()
                ));
            } else if started.elapsed() > Duration::from_secs(20) {
                failures.push(format!("{kind} round {round} took {:?}", started.elapsed()));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_damaged_zip_member_name_or_header_does_not_panic() {
    let mut rng = Rng(7);
    let good = fixtures::xlsx(&[("S", vec![vec![X::S("a")], vec![X::N("1")]])]);
    for _ in 0..200 {
        let d = Dir::new();
        d.put("b.xlsx", damage(&good, &mut rng));
        std::panic::catch_unwind(|| check(run(json!({"path": d.s(), "mode": "describe"})), "xlsx"))
            .expect("no panic");
    }
}

#[test]
fn broken_files_in_a_folder_are_reported_one_by_one_and_the_good_ones_still_work() {
    let d = Dir::new();
    d.put("good.csv", "a\n1\n");
    d.put("corrupt_footer.parquet", {
        let mut b = fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY);
        let n = b.len();
        b[n - 40..n - 8].fill(0xAB);
        b
    });
    d.put(
        "truncated.parquet",
        &fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY)[..60],
    );
    d.put("not_parquet.parquet", "name,score\nAna,9\n");
    d.put(
        "truncated.xlsx",
        &fixtures::xlsx(&[("S", vec![vec![X::S("a")]])])[..100],
    );
    d.put("bomb.xlsx", fixtures::zip_bomb());
    d.put("blob.bin", [0u8, 1, 2, 255, 0, 7]);
    d.put("unclosed.csv", "a,b\n1,\"never closed\n2,3\n");
    d.put("truncated.json", "[{\"a\":1},{\"a\":");
    d.put("empty.csv", "");
    d.put("header_only.csv", "a,b\n");
    let r = run(json!({"path": d.s()})).unwrap();
    let by_name = |n: &str| {
        r["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == n)
            .cloned()
            .unwrap_or_else(|| panic!("no table {n}: {r}"))
    };
    let err = |n: &str| {
        by_name(n)["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{n} has no error"))
            .to_string()
    };
    assert_eq!(by_name("good")["row_count"], 1);
    assert!(err("corrupt_footer").contains("is not a readable Parquet file"));
    assert!(
        err("truncated").contains("is not valid JSON at line"),
        "{}",
        err("truncated")
    );
    assert!(
        err("truncated_2").contains("is not a readable Parquet file"),
        "{}",
        err("truncated_2")
    );
    assert!(
        err("bomb_S").contains("decompression bomb"),
        "{}",
        err("bomb_S")
    );
    assert!(err("empty").contains("has no rows to read"));
    assert_eq!(by_name("header_only")["row_count"], 0);
    assert!(
        err("unclosed").contains("the quoted field on line 2 is never closed"),
        "{}",
        err("unclosed")
    );
    let skipped = r["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        skipped.contains("blob.bin: a binary file that is not a supported data format"),
        "{skipped}"
    );
    assert!(
        skipped.contains(
            "not_parquet.parquet: named like Parquet but does not start like a Parquet file"
        ),
        "{skipped}"
    );

    let e = query(&d, "SELECT * FROM bomb_S").unwrap_err();
    assert!(e.contains("decompression bomb"), "{e}");
    let e = query(&d, "SELECT * FROM unclosed").unwrap_err();
    assert!(
        e.contains("the quoted field on line 2 is never closed"),
        "{e}"
    );
    let e = query(&d, "SELECT * FROM truncated").unwrap_err();
    assert!(e.contains("is not valid JSON at line"), "{e}");
    assert_eq!(
        query(&d, "SELECT a FROM good").unwrap()["rows"],
        json!([[1]])
    );
}

#[test]
fn a_file_with_ten_thousand_columns_is_queried_and_listed() {
    let d = Dir::new();
    let header: Vec<String> = (0..10_000).map(|i| format!("c{i}")).collect();
    let row: Vec<String> = (0..10_000).map(|i| (i * 2).to_string()).collect();
    d.put(
        "w.csv",
        format!(
            "{}\n{}\n{}\n",
            header.join(","),
            row.join(","),
            row.join(",")
        ),
    );
    let r = query(&d, "SELECT c9999, c0, c5000 FROM w").unwrap();
    assert_eq!(r["rows"], json!([[19998, 0, 10000], [19998, 0, 10000]]));
    let all = query(&d, "SELECT * FROM w").unwrap();
    assert_eq!(all["columns"].as_array().unwrap().len(), 10_000);
    assert_eq!(all["rows"][0].as_array().unwrap().len(), 10_000);
    assert_eq!(
        query(&d, "SELECT count(*) AS n FROM w").unwrap()["rows"],
        json!([[2]])
    );
}

#[test]
fn a_row_over_16_mib_is_refused_not_buffered() {
    let d = Dir::new();
    d.put(
        "big.csv",
        format!("a,b\n1,\"{}\"\n", "x".repeat(17 * 1024 * 1024)),
    );
    let e = run(json!({"path": d.s(), "sql": "SELECT count(*) FROM big"})).unwrap_err();
    assert!(
        e.contains("a row on line 2 is over 16 MiB; check the quoting and the delimiter"),
        "{e}"
    );
}

#[test]
fn deeply_nested_json_is_refused_with_a_sentence() {
    let d = Dir::new();
    d.put(
        "deep.json",
        format!("{}1{}", "[".repeat(300), "]".repeat(300)),
    );
    let e = run(json!({"path": d.s(), "sql": "SELECT * FROM deep"})).unwrap_err();
    assert!(e.contains("nests JSON deeper than 128 levels"), "{e}");
    let ok = Dir::new();
    ok.put(
        "d.json",
        format!("[{{\"a\":{}1{}}}]", "{\"k\":".repeat(40), "}".repeat(40)),
    );
    let r = run(json!({"path": ok.s(), "sql": "SELECT count(*) AS n FROM d"})).unwrap();
    assert_eq!(r["rows"], json!([[1]]));
}

#[test]
fn mixed_types_ragged_rows_and_duplicate_names_still_answer() {
    let d = Dir::new();
    d.put("mixed.csv", "v\n1\n2\nthree\n");
    d.put("ragged.csv", "a,b,c\n1,2,3\n4,5\n6,7,8,9\n");
    d.put("dupes.csv", "a,a,a\n1,2,3\n");
    let r = query(&d, "SELECT v FROM mixed ORDER BY v").unwrap();
    assert_eq!(r["columns"][0]["type"], "text");
    assert_eq!(r["rows"], json!([["1"], ["2"], ["three"]]));
    let r = query(&d, "SELECT * FROM ragged").unwrap();
    assert_eq!(r["rows"], json!([[1, 2, 3], [4, 5, null], [6, 7, 8]]));
    assert_eq!(r["warnings"].as_array().unwrap().len(), 1);
    let r = query(&d, "SELECT a, a_2, a_3 FROM dupes").unwrap();
    assert_eq!(r["rows"], json!([[1, 2, 3]]));
    assert_eq!(
        r["warnings"],
        json!(["2 repeated column names got a number added (name_2, name_3)"])
    );
}

#[test]
fn paths_that_leave_the_folder_are_refused_end_to_end() {
    let outside = Dir::new();
    outside.put("secret.csv", "k\ntop\n");
    let d = Dir::new();
    d.put("ok.csv", "x\n1\n");
    for bad in ["../secret.csv", "/etc/passwd", "sub/../../secret.csv", ""] {
        let e = run(json!({"path": d.s(), "files": [bad], "mode": "tables"})).unwrap_err();
        assert!(
            e.contains("not a path inside the folder") || e.contains("cannot read"),
            "{bad:?}: {e}"
        );
        assert!(!e.contains("top"));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path().join("secret.csv"), d.path().join("leak.csv"))
            .unwrap();
        let e = run(json!({"path": d.s(), "files": ["leak.csv"], "mode": "tables"})).unwrap_err();
        assert!(e.contains("a link that leads outside the folder"), "{e}");
        let listed = run(json!({"path": d.s(), "mode": "tables"})).unwrap();
        assert_eq!(listed["tables"].as_array().unwrap().len(), 1);
        assert!(
            listed["warnings"][0]
                .as_str()
                .unwrap()
                .contains("leak.csv: a link that leads outside the folder")
        );
    }
}
