//! What the read-side mechanisms cost: projected reads, value-validated reads,
//! write-free commits and the page cursor.
//!
//! Four questions, each a median over many repetitions:
//!
//! * `get_parts` against `get`: the price of recording a part set.
//! * `cursor` against `scan_stream` over the same range.
//! * A commit with many reads, at the level that compares sequences and at the
//!   level that compares values, uncontended and with every key rewritten with
//!   the bytes it held. The second is the path the value rule exists for.
//! * A commit that writes nothing against one that writes.
//!
//! Every count is asserted: a scan that silently returned the wrong rows would
//! otherwise report as a fast one.

mod common;

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use regolith::{
    IsolationLevel, MergeOperator, OptimisticTransactionDb, ScanCheck, ScanDirection, Transaction,
    TxnOptions,
};

/// Appends its operands, and says an operand touches none of the parts a read
/// names: a commit whose reads were projected asks `touches` of every newer
/// operand of every key it read and commits. With the default contract (every
/// part touched) the commit would be refused at the first key it checks, and
/// a row of `n` reads would time one check.
struct Concat;

impl MergeOperator for Concat {
    fn name(&self) -> &'static str {
        "concat"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands.iter().for_each(|o| out.extend_from_slice(o));
        Some(out)
    }

    fn touches(&self, _: &[u8], _: &[u8], _: &[u32]) -> bool {
        false
    }
}

const VALUE_LEN: usize = 64;

fn open(tag: &str, keys: usize) -> (common::TempDb, OptimisticTransactionDb) {
    let tmp = common::TempDb::new(tag);
    let opts = common::default_opts().merge_operator(Some(Arc::new(Concat)));
    let db =
        OptimisticTransactionDb::open(tmp.path(), opts).unwrap_or_else(|e| panic!("open: {e}"));
    let value = [7u8; VALUE_LEN];
    for i in 0..keys {
        db.db()
            .put(&common::key(i as u64), &value)
            .unwrap_or_else(|e| panic!("seed: {e}"));
    }
    (tmp, db)
}

fn at(level: IsolationLevel) -> TxnOptions {
    TxnOptions::new().isolation(level)
}

/// Median nanoseconds per call of `read` over `keys` keys, `reps` times.
fn per_read(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    keys: usize,
    reps: usize,
    read: impl Fn(&Transaction, &[u8]),
) -> f64 {
    let names: Vec<Vec<u8>> = (0..keys).map(|i| common::key(i as u64)).collect();
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let txn = db.begin(&at(level));
        let started = Instant::now();
        for name in &names {
            read(&txn, name);
        }
        samples.push(started.elapsed().as_secs_f64() * 1e9 / keys as f64);
        txn.rollback();
    }
    common::median(&mut samples)
}

/// Median microseconds of a commit that read `keys` keys and wrote one.
fn commit_us(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    keys: usize,
    reps: usize,
    read: impl Fn(&Transaction, &[u8]),
    between: impl Fn(&regolith::Db, &[Vec<u8>]),
    write: bool,
) -> f64 {
    let names: Vec<Vec<u8>> = (0..keys).map(|i| common::key(i as u64)).collect();
    let mut samples = Vec::with_capacity(reps);
    for rep in 0..reps {
        let txn = db.begin(&at(level));
        for name in &names {
            read(&txn, name);
        }
        between(db.db(), &names);
        if write {
            txn.put(b"zzz/own", &rep.to_le_bytes())
                .unwrap_or_else(|e| panic!("put: {e}"));
        }
        let started = Instant::now();
        let outcome = txn.commit();
        samples.push(started.elapsed().as_secs_f64() * 1e6);
        // Every row times a commit that lands: one refused would time a
        // different path, stopped at the first key it checked.
        outcome.unwrap_or_else(|e| panic!("{level:?}: commit {rep}: {e}"));
    }
    common::median(&mut samples)
}

fn main() {
    let quick = common::args()
        .iter()
        .any(|a| a == "--quick" || a == "--test");
    let (reps, reads) = if quick { (10, 256) } else { (200, 4096) };
    let mut rows = Vec::new();
    let mut report = |name: &str, value: f64, unit: &str| {
        println!("{name:<58} {value:>12.2} {unit}");
        rows.push(format!(
            "{{\"name\":\"{name}\",\"value\":{value:.2},\"unit\":\"{unit}\"}}"
        ));
    };

    let (_tmp, db) = open("read-rules-reads", reads);
    println!("reads: {reads} distinct keys per transaction, median of {reps}");
    let defra = IsolationLevel::DefraLevel;
    report(
        "get (defralevel)",
        per_read(&db, defra, reads, reps, |t, k| {
            black_box(t.get(k).unwrap());
        }),
        "ns/read",
    );
    report(
        "get_slice (defralevel)",
        per_read(&db, defra, reads, reps, |t, k| {
            black_box(t.get_slice(k).unwrap());
        }),
        "ns/read",
    );
    report(
        "get_parts, 2 parts (defralevel)",
        per_read(&db, defra, reads, reps, |t, k| {
            black_box(t.get_parts(k, &[1, 3]).unwrap());
        }),
        "ns/read",
    );
    report(
        "get_parts, no parts (defralevel)",
        per_read(&db, defra, reads, reps, |t, k| {
            black_box(t.get_parts(k, &[]).unwrap());
        }),
        "ns/read",
    );
    report(
        "get_parts (repeatable-read: a plain get)",
        per_read(&db, IsolationLevel::RepeatableRead, reads, reps, |t, k| {
            black_box(t.get_parts(k, &[1, 3]).unwrap());
        }),
        "ns/read",
    );
    report(
        "get (repeatable-read)",
        per_read(&db, IsolationLevel::RepeatableRead, reads, reps, |t, k| {
            black_box(t.get(k).unwrap());
        }),
        "ns/read",
    );

    // Scans.
    let scan_keys = if quick { 2048 } else { 32_768 };
    let (_scan_tmp, scan_db) = open("read-rules-scan", scan_keys);
    let scan_reps = if quick { 5 } else { 30 };
    println!("scans: {scan_keys} keys of {VALUE_LEN} bytes, median of {scan_reps}");
    let scan_ns = |walk: &dyn Fn(&Transaction) -> usize| {
        let mut samples = Vec::with_capacity(scan_reps);
        for _ in 0..scan_reps {
            let txn = scan_db.begin(&at(IsolationLevel::SnapshotIsolation));
            let started = Instant::now();
            let seen = walk(&txn);
            samples.push(started.elapsed().as_secs_f64() * 1e9 / scan_keys as f64);
            assert_eq!(seen, scan_keys, "the scan must yield every key");
        }
        common::median(&mut samples)
    };
    report(
        "scan_stream",
        scan_ns(&|txn| txn.scan_stream(None, None).filter(Result::is_ok).count()),
        "ns/entry",
    );
    for (name, check) in [
        ("cursor, 64 KiB pages, stretch", ScanCheck::Stretch),
        ("cursor, 64 KiB pages, range", ScanCheck::Range),
    ] {
        report(
            name,
            scan_ns(&|txn| {
                let mut cursor = txn.cursor(None, None, ScanDirection::Forward, check.clone());
                let mut seen = 0;
                loop {
                    let page = cursor.next_page(txn, 64 * 1024).unwrap();
                    seen += page.entries.len();
                    if page.done {
                        return seen;
                    }
                }
            }),
            "ns/entry",
        );
    }

    // Commits with many reads.
    println!("commits: a transaction reads n keys and writes one");
    let commit_reps = if quick { 10 } else { 100 };
    let sizes: &[usize] = if quick {
        &[1, 64, 256]
    } else {
        &[1, 64, 1024, 4096]
    };
    for &n in sizes {
        let none = |_: &regolith::Db, _: &[Vec<u8>]| {};
        let get = |t: &Transaction, k: &[u8]| {
            black_box(t.get(k).unwrap());
        };
        for (level, name) in [
            (IsolationLevel::RepeatableRead, "sequence"),
            (IsolationLevel::DefraLevel, "value"),
        ] {
            report(
                &format!("commit {n:>5} reads, {name} rule, nothing newer"),
                commit_us(&db, level, n, commit_reps, get, none, true),
                "us",
            );
        }
        report(
            &format!("commit {n:>5} get_parts reads, nothing newer"),
            commit_us(
                &db,
                defra,
                n,
                commit_reps,
                |t, k| {
                    black_box(t.get_parts(k, &[1, 3]).unwrap());
                },
                none,
                true,
            ),
            "us",
        );
        // The two rows below write between the reads and the commit, each to a
        // database of its own, so the versions and operands they leave are not
        // read by any other row.
        //
        // Every key rewritten with the bytes it held: the value rule's path.
        let rewrite = |db: &regolith::Db, names: &[Vec<u8>]| {
            let value = [7u8; VALUE_LEN];
            names.iter().for_each(|k| db.put(k, &value).unwrap());
        };
        let (_rewrite_tmp, rewrite_db) = open(&format!("read-rules-rewrite-{n}"), n);
        report(
            &format!("commit {n:>5} reads, every key rewritten identically"),
            commit_us(
                &rewrite_db,
                defra,
                n,
                commit_reps.min(20),
                get,
                rewrite,
                true,
            ),
            "us",
        );
        // Every key given an operand: the projected rule asks `touches`.
        let operand = |db: &regolith::Db, names: &[Vec<u8>]| {
            names.iter().for_each(|k| db.merge(k, b"+").unwrap());
        };
        let (_operand_tmp, operand_db) = open(&format!("read-rules-operand-{n}"), n);
        report(
            &format!("commit {n:>5} get_parts reads, every key has an operand"),
            commit_us(
                &operand_db,
                defra,
                n,
                commit_reps.min(20),
                |t, k| {
                    black_box(t.get_parts(k, &[1, 3]).unwrap());
                },
                operand,
                true,
            ),
            "us",
        );
    }

    // Write-free against writing.
    println!("commits that write nothing");
    for &n in &[1usize, 64] {
        let get = |t: &Transaction, k: &[u8]| {
            black_box(t.get(k).unwrap());
        };
        let none = |_: &regolith::Db, _: &[Vec<u8>]| {};
        for (write, name) in [(false, "write-free"), (true, "one write")] {
            report(
                &format!("commit {n:>5} reads, defralevel, {name}"),
                commit_us(&db, defra, n, commit_reps, get, none, write),
                "us",
            );
        }
        report(
            &format!("commit {n:>5} reads, repeatable-read, no write"),
            commit_us(
                &db,
                IsolationLevel::RepeatableRead,
                n,
                commit_reps,
                get,
                none,
                false,
            ),
            "us",
        );
    }
    common::write_family(
        "read_rules",
        &format!(
            "{{\"quick\":{quick},\"reps\":{reps},\"reads\":{reads},\"rows\":[{}]}}",
            rows.join(",")
        ),
    );
}
