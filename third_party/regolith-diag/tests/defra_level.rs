//! `IsolationLevel::DefraLevel`: RepeatableRead, relaxed only where the
//! keyspace rules a conflict out.
//!
//! Each relaxation is shown next to the RepeatableRead outcome it replaces,
//! and next to the conflicts it must keep.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::prelude::*;
use regolith::{PerfContext, PerfLevel, TransactionError, TxnOptions};

mod common;

/// Sums big-endian i64 deltas.
struct CounterMerge;

impl MergeOperator for CounterMerge {
    fn name(&self) -> &'static str {
        "counter"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut total: i64 = match base {
            Some(bytes) if bytes.len() == 8 => i64::from_be_bytes(bytes.try_into().unwrap()),
            Some(_) => return None,
            None => 0,
        };
        for operand in operands {
            if operand.len() != 8 {
                return None;
            }
            total = total.wrapping_add(i64::from_be_bytes((*operand).try_into().unwrap()));
        }
        Some(total.to_be_bytes().to_vec())
    }
}

/// Keys under `h/` are added under fresh names and never contended.
struct HeadPrefix;

impl KeyClassifier for HeadPrefix {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"h/") {
            KeyClass::CommutativePrefix { len: 2 }
        } else {
            KeyClass::Ordinary
        }
    }
}

/// Open under `options`, with the same key policy as `open`.
fn open_with(dir: &std::path::Path, options: Options) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, options)
        .unwrap()
        .with_policy(Arc::new(HeadPrefix))
}

fn open(dir: &std::path::Path) -> OptimisticTransactionDb {
    open_with(
        dir,
        Options::default().merge_operator(Some(Arc::new(CounterMerge))),
    )
}

/// Small blocks, so a key's merge chain spans many of them, and no
/// background compaction to fold the chain away.
fn block_spanning() -> Options {
    Options::default()
        .merge_operator(Some(Arc::new(CounterMerge)))
        .block_size(512)
        .max_background_compactions(0)
}

/// `block_spanning` with a partitioned index of small leaves.
fn block_spanning_partitioned() -> Options {
    block_spanning()
        .partitioned_index(true)
        .metadata_block_size(512)
}

/// Both index shapes, as constructors so a test can open several databases
/// per shape.
const INDEX_SHAPES: [fn() -> Options; 2] = [block_spanning, block_spanning_partitioned];

fn counter(db: &OptimisticTransactionDb) -> i64 {
    i64::from_be_bytes(
        db.db().get(b"counter").unwrap().unwrap()[..]
            .try_into()
            .unwrap(),
    )
}

fn conflicted<T>(result: TxResult<T>) -> bool {
    matches!(result, Err(TransactionError::Conflict { .. }))
}

#[test]
fn point_reads_are_validated_as_at_repeatable_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"old").unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.get(b"k").unwrap();
    db.db().put(b"k", b"new").unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    assert!(conflicted(tx.commit()));
}

#[test]
fn concurrent_blind_merges_commit_where_repeatable_read_aborts() {
    for flush in [false, true] {
        for (level, both_commit) in [
            (IsolationLevel::DefraLevel, true),
            (IsolationLevel::RepeatableRead, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let db = open(dir.path());
            db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

            let first = db.begin(&TxnOptions::new().isolation(level));
            let second = db.begin(&TxnOptions::new().isolation(level));
            first.merge(b"counter", &1i64.to_be_bytes()).unwrap();
            second.merge(b"counter", &1i64.to_be_bytes()).unwrap();
            first.commit().unwrap();
            if flush {
                db.db().flush().unwrap();
            }
            let second = second.commit();
            assert_eq!(
                common::committed(&second),
                both_commit,
                "{level:?} flush={flush}: {second:?}"
            );
            assert_eq!(counter(&db), if both_commit { 2 } else { 1 });
        }
    }
}

#[test]
fn eight_threads_of_blind_merges_never_conflict_and_lose_nothing() {
    const THREADS: i64 = 8;
    const MERGES: i64 = 200;
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for i in 0..MERGES {
                    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                    tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
                    if let Err(e) = tx.commit() {
                        panic!("a blind merge never conflicts: {e:?}");
                    }
                    if i % 50 == 0 {
                        db.db().flush().unwrap();
                    }
                }
            });
        }
    });
    assert_eq!(counter(&db), THREADS * MERGES);
}

/// A write that replaces a key outright instead of building on it.
#[derive(Clone, Copy, Debug)]
enum Replacement {
    Put,
    Delete,
    RangeDelete,
}

impl Replacement {
    fn apply(self, db: &OptimisticTransactionDb) {
        match self {
            Self::Put => db.db().put(b"counter", &5i64.to_be_bytes()).unwrap(),
            Self::Delete => db.db().delete(b"counter").unwrap(),
            Self::RangeDelete => db.db().delete_range(b"c", b"d").unwrap(),
        }
    }
}

#[test]
fn a_blind_merge_still_conflicts_with_a_newer_replacement() {
    for replacement in [
        Replacement::Put,
        Replacement::Delete,
        Replacement::RangeDelete,
    ] {
        for flush in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let db = open(dir.path());
            db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

            let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
            tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
            replacement.apply(&db);
            // Operands on top of the replacement do not hide it.
            db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
            if flush {
                db.db().flush().unwrap();
            }
            assert!(conflicted(tx.commit()), "{replacement:?} flush={flush}");
        }
    }
}

#[test]
fn a_merge_into_a_key_the_transaction_read_is_not_blind() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.get(b"counter").unwrap();
    tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
    assert!(conflicted(tx.commit()));
}

#[test]
fn a_merge_beside_a_put_of_the_same_key_is_not_blind() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.put(b"counter", &3i64.to_be_bytes()).unwrap();
    tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
    assert!(conflicted(tx.commit()));
}

/// Scan the head prefix, then write the marker every writer writes. The
/// marker sorts between the heads, so it lies inside the stretch walked.
fn supersede(tx: &Transaction, start: &[u8], end: &[u8]) {
    let walked: Vec<_> = tx.scan_stream(Some(start), Some(end)).collect();
    assert!(!walked.is_empty());
    tx.put(b"h/m", b"superseded").unwrap();
}

#[test]
fn an_identical_write_inside_a_scanned_commutative_prefix_commits() {
    for (level, both_commit) in [
        (IsolationLevel::DefraLevel, true),
        (IsolationLevel::RepeatableRead, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        db.db().put(b"h/a", b"cid").unwrap();
        db.db().put(b"h/z", b"cid").unwrap();

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));
        supersede(&first, b"h/", b"h0");
        supersede(&second, b"h/", b"h0");
        first.commit().unwrap();
        let second = second.commit();
        assert_eq!(
            common::committed(&second),
            both_commit,
            "{level:?}: {second:?}"
        );
    }
}

#[test]
fn a_scan_leaving_the_commutative_prefix_is_still_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"h/a", b"cid").unwrap();
    db.db().put(b"i/other", b"x").unwrap();

    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    supersede(&first, b"h/", b"j");
    supersede(&second, b"h/", b"j");
    first.commit().unwrap();
    assert!(conflicted(second.commit()));
}

#[test]
fn without_a_policy_scans_are_recorded_as_at_repeatable_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    db.db().put(b"h/a", b"cid").unwrap();
    db.db().put(b"h/z", b"cid").unwrap();

    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    supersede(&first, b"h/", b"h0");
    supersede(&second, b"h/", b"h0");
    first.commit().unwrap();
    assert!(conflicted(second.commit()));
}

/// The value of the counter under `key`.
fn counter_at(db: &OptimisticTransactionDb, key: &[u8]) -> i64 {
    i64::from_be_bytes(db.db().get(key).unwrap().unwrap()[..].try_into().unwrap())
}

#[test]
fn a_merge_made_before_a_scan_of_a_commutative_prefix_stays_blind() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        db.db().put(b"h/a", b"cid").unwrap();
        db.db().put(b"h/m", &0i64.to_be_bytes()).unwrap();
        db.db().put(b"h/z", b"cid").unwrap();
        if flush {
            db.db().flush().unwrap();
        }

        let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        for tx in [&first, &second] {
            tx.merge(b"h/m", &1i64.to_be_bytes()).unwrap();
            let walked: Vec<_> = tx
                .scan_stream(Some(b"h/"), Some(b"h0"))
                .map(Result::unwrap)
                .collect();
            assert_eq!(walked.len(), 3, "flush={flush}");
            assert_eq!(
                walked[1].1.to_vec(),
                1i64.to_be_bytes(),
                "flush={flush}: the scan yields the key with the transaction's operand applied"
            );
        }
        first.commit().unwrap();
        let second = second.commit();
        assert!(second.is_ok(), "flush={flush}: {second:?}");
        assert_eq!(counter_at(&db, b"h/m"), 2, "flush={flush}");
    }
}

/// A plain write to a key that lands after a transaction's snapshot.
#[derive(Clone, Copy, Debug)]
enum Newer {
    Put,
    Merge,
    Delete,
}

impl Newer {
    const ALL: [Self; 3] = [Self::Put, Self::Merge, Self::Delete];

    fn apply(self, db: &OptimisticTransactionDb, key: &[u8]) {
        match self {
            Self::Put => db.db().put(key, &5i64.to_be_bytes()).unwrap(),
            Self::Merge => db.db().merge(key, &1i64.to_be_bytes()).unwrap(),
            Self::Delete => db.db().delete(key).unwrap(),
        }
    }
}

#[test]
fn a_merged_key_inside_a_scanned_stretch_is_a_read_unless_the_stretch_is_commutative() {
    for flush in [false, true] {
        for newer in Newer::ALL {
            // In a commutative prefix the merge stays blind, so only a newer
            // replacement conflicts with it. Anywhere else the scan walked the
            // key before the merge, so any newer write does.
            for (prefix, key, conflicts) in [
                (&b"h/"[..], &b"h/m"[..], !matches!(newer, Newer::Merge)),
                (&b"o/"[..], &b"o/m"[..], true),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let db = open(dir.path());
                db.db().put(&[prefix, &b"a"[..]].concat(), b"x").unwrap();
                db.db().put(key, &0i64.to_be_bytes()).unwrap();
                db.db().put(&[prefix, &b"z"[..]].concat(), b"x").unwrap();

                let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                tx.merge(key, &1i64.to_be_bytes()).unwrap();
                let end = [&prefix[..1], &b"0"[..]].concat();
                assert_eq!(tx.scan_stream(Some(prefix), Some(&end)).count(), 3);
                newer.apply(&db, key);
                if flush {
                    db.db().flush().unwrap();
                }
                let committed = tx.commit();
                let name = format!("{newer:?} flush={flush} {}", String::from_utf8_lossy(key));
                if conflicts {
                    match committed {
                        Err(TransactionError::Conflict(conflict)) => {
                            assert_eq!(conflict.key(), key, "{name}");
                        }
                        other => panic!("{name}: expected a conflict, got {other:?}"),
                    }
                } else {
                    assert!(committed.is_ok(), "{name}: {committed:?}");
                }
            }
        }
    }
}

/// Commit a blind merge onto a key with `operands` flushed merges beneath
/// it, after one more merge landed since the transaction's snapshot. Returns
/// the block cache lookups the commit made and the bytes of SSTable the
/// chain occupies.
fn blind_merge_commit(options: Options, operands: i64) -> (u64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let db = open_with(dir.path(), options);
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();
    for _ in 0..operands {
        db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
    }
    db.db().flush().unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();

    PerfContext::set_level(PerfLevel::EnableCount);
    PerfContext::reset();
    let committed = tx.commit();
    let lookups = PerfContext::capture().block_cache_lookup_count;
    PerfContext::set_level(PerfLevel::Disable);
    committed.expect("a blind merge commits beside newer operands");

    // Read after the capture: a read walks the whole chain.
    assert_eq!(counter(&db), operands + 2);
    let sst_bytes = db
        .db()
        .get_int_property("regolith.total-sst-files-size")
        .unwrap();
    (lookups, sst_bytes)
}

#[test]
fn a_blind_merge_commit_reads_only_what_landed_since_its_snapshot() {
    for options in INDEX_SHAPES {
        let (short_lookups, short_bytes) = blind_merge_commit(options(), 200);
        let (long_lookups, long_bytes) = blind_merge_commit(options(), 2000);

        // Ten times the chain over many more blocks, so a walk down the
        // chain could not read the same number of them.
        assert!(
            long_bytes > 4 * short_bytes,
            "{long_bytes} bytes of table for 2000 operands against {short_bytes} for 200"
        );
        assert!(short_lookups > 0, "the commit never consulted the table");
        assert_eq!(
            long_lookups, short_lookups,
            "the commit reads the block of the newest flushed operand, however long the chain"
        );
    }
}

#[test]
fn a_replacement_beneath_operands_spanning_blocks_still_conflicts() {
    for replacement in [
        Replacement::Put,
        Replacement::Delete,
        Replacement::RangeDelete,
    ] {
        for (shape, options) in INDEX_SHAPES.into_iter().enumerate() {
            let dir = tempfile::tempdir().unwrap();
            let db = open_with(dir.path(), options());
            db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

            let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
            tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
            replacement.apply(&db);
            for _ in 0..2000 {
                db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
            }
            db.db().flush().unwrap();
            assert!(
                conflicted(tx.commit()),
                "{replacement:?} index shape {shape}"
            );
        }
    }
}
