//! A transaction reads its own buffered merges.
//!
//! A read of a key the transaction merged into answers what the merge
//! operator makes of the value the transaction reads for the key, then its
//! own operands in the order it made them. A put or a delete of the key
//! resets that value, so the operands buffered before it no longer count,
//! and a commit stores exactly what the read answered. Each case runs on
//! both transaction flavours.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::sync::Arc;

use regolith::{
    Db, Error, IsolationLevel, MergeOperator, OptimisticTransactionDb, Options, Transaction,
    TransactionDb, TransactionError, TxnOptions,
};

/// Sums big-endian i64 deltas onto the base.
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

/// Appends every operand to the base in order, so the order the operands
/// were folded in shows in the result.
struct AppendMerge;

impl MergeOperator for AppendMerge {
    fn name(&self) -> &'static str {
        "append"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }
}

fn counting() -> Options {
    Options::default().merge_operator(Some(Arc::new(CounterMerge)))
}

fn appending() -> Options {
    Options::default().merge_operator(Some(Arc::new(AppendMerge)))
}

/// `counting`, with a write buffer that never builds an index, so a lookup
/// always walks the list.
fn counting_unindexed() -> Options {
    counting().transaction_keys_inline(0)
}

/// `counting` with both buffer shapes: indexed past the default size, and
/// never.
const BUFFER_SHAPES: [fn() -> Options; 2] = [counting, counting_unindexed];

fn delta(n: i64) -> [u8; 8] {
    n.to_be_bytes()
}

/// Either transaction flavour behind one type.
enum Flavour {
    Optimistic(OptimisticTransactionDb),
    Pessimistic(TransactionDb),
}

impl Flavour {
    fn open(optimistic: bool, dir: &Path, options: Options) -> Self {
        if optimistic {
            Self::Optimistic(OptimisticTransactionDb::open(dir, options).unwrap())
        } else {
            Self::Pessimistic(TransactionDb::open(dir, options).unwrap())
        }
    }

    fn begin(&self) -> Transaction {
        match self {
            Self::Optimistic(db) => db.begin(&TxnOptions::new()),
            Self::Pessimistic(db) => db.begin(&TxnOptions::new()),
        }
    }

    fn db(&self) -> &Db {
        match self {
            Self::Optimistic(db) => db.db(),
            Self::Pessimistic(db) => db.db(),
        }
    }
}

/// Run `check` against a fresh database of each flavour.
fn each_flavour(options: fn() -> Options, check: impl Fn(&Flavour)) {
    for optimistic in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        check(&Flavour::open(optimistic, dir.path(), options()));
    }
}

fn counter(bytes: Option<Vec<u8>>) -> Option<i64> {
    bytes.map(|bytes| i64::from_be_bytes(bytes[..].try_into().unwrap()))
}

/// What the transaction reads for `key` through each of its point reads,
/// which must agree.
fn reads(tx: &Transaction, key: &[u8]) -> Option<Vec<u8>> {
    let got = tx.get(key).unwrap();
    assert_eq!(
        tx.get_slice(key).unwrap().map(|slice| slice.to_vec()),
        got,
        "get_slice disagrees with get"
    );
    assert_eq!(
        tx.get_for_update(key).unwrap(),
        got,
        "get_for_update disagrees with get"
    );
    got
}

#[test]
fn two_merges_into_a_counter_read_back_as_the_base_plus_both() {
    each_flavour(counting, |flavour| {
        flavour.db().put(b"k", &delta(10)).unwrap();
        let tx = flavour.begin();
        tx.merge(b"k", &delta(1)).unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(11));
        tx.merge(b"k", &delta(1)).unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(12));
        assert_eq!(
            counter(flavour.db().get(b"k").unwrap()),
            Some(10),
            "nothing is visible outside before the commit"
        );
        tx.commit().unwrap();
        assert_eq!(counter(flavour.db().get(b"k").unwrap()), Some(12));
    });
}

#[test]
fn a_merge_into_an_absent_key_starts_from_nothing() {
    each_flavour(counting, |flavour| {
        let tx = flavour.begin();
        assert_eq!(reads(&tx, b"k"), None);
        tx.merge(b"k", &delta(7)).unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(7));
        tx.commit().unwrap();
        assert_eq!(counter(flavour.db().get(b"k").unwrap()), Some(7));
    });
}

#[test]
fn a_merge_made_after_a_put_applies_to_the_put() {
    each_flavour(counting, |flavour| {
        flavour.db().put(b"k", &delta(10)).unwrap();
        let tx = flavour.begin();
        tx.put(b"k", &delta(100)).unwrap();
        tx.merge(b"k", &delta(1)).unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(101));
        tx.commit().unwrap();
        assert_eq!(counter(flavour.db().get(b"k").unwrap()), Some(101));
    });
}

#[test]
fn a_put_made_after_a_merge_discards_the_merge() {
    each_flavour(counting, |flavour| {
        flavour.db().put(b"k", &delta(10)).unwrap();
        let tx = flavour.begin();
        tx.merge(b"k", &delta(1)).unwrap();
        tx.put(b"k", &delta(100)).unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(100));
        tx.commit().unwrap();
        assert_eq!(
            counter(flavour.db().get(b"k").unwrap()),
            Some(100),
            "the commit stores what the transaction read"
        );
    });
}

#[test]
fn a_delete_resets_the_base_like_a_put() {
    each_flavour(counting, |flavour| {
        flavour.db().put(b"gone", &delta(10)).unwrap();
        flavour.db().put(b"reborn", &delta(10)).unwrap();
        let tx = flavour.begin();

        tx.merge(b"gone", &delta(1)).unwrap();
        tx.delete(b"gone").unwrap();
        assert_eq!(reads(&tx, b"gone"), None);

        tx.delete(b"reborn").unwrap();
        tx.merge(b"reborn", &delta(5)).unwrap();
        assert_eq!(counter(reads(&tx, b"reborn")), Some(5));

        tx.commit().unwrap();
        assert_eq!(flavour.db().get(b"gone").unwrap(), None);
        assert_eq!(counter(flavour.db().get(b"reborn").unwrap()), Some(5));
    });
}

#[test]
fn puts_and_merges_fold_in_the_order_they_were_made() {
    each_flavour(appending, |flavour| {
        flavour.db().put(b"k", b"x").unwrap();
        let tx = flavour.begin();
        tx.merge(b"k", b"a").unwrap();
        assert_eq!(reads(&tx, b"k").as_deref(), Some(&b"xa"[..]));
        tx.put(b"k", b"P").unwrap();
        tx.merge(b"k", b"b").unwrap();
        tx.merge(b"k", b"c").unwrap();
        assert_eq!(reads(&tx, b"k").as_deref(), Some(&b"Pbc"[..]));
        tx.delete(b"k").unwrap();
        tx.merge(b"k", b"d").unwrap();
        tx.merge(b"k", b"e").unwrap();
        assert_eq!(reads(&tx, b"k").as_deref(), Some(&b"de"[..]));
        tx.commit().unwrap();
        assert_eq!(flavour.db().get(b"k").unwrap().as_deref(), Some(&b"de"[..]));
    });
}

#[test]
fn merges_into_different_keys_do_not_mix() {
    each_flavour(appending, |flavour| {
        flavour.db().put(b"a", b"1").unwrap();
        flavour.db().put(b"b", b"2").unwrap();
        let tx = flavour.begin();
        tx.merge(b"a", b"x").unwrap();
        tx.merge(b"b", b"y").unwrap();
        tx.merge(b"a", b"z").unwrap();
        assert_eq!(reads(&tx, b"a").as_deref(), Some(&b"1xz"[..]));
        assert_eq!(reads(&tx, b"b").as_deref(), Some(&b"2y"[..]));
        assert_eq!(reads(&tx, b"c"), None);
    });
}

#[test]
fn a_failing_merge_is_an_error_not_a_stale_value() {
    each_flavour(counting, |flavour| {
        flavour.db().put(b"k", &delta(10)).unwrap();
        let tx = flavour.begin();
        tx.merge(b"k", b"not a delta").unwrap();
        for result in [
            tx.get(b"k").map(|_| ()),
            tx.get_slice(b"k").map(|_| ()),
            tx.get_for_update(b"k").map(|_| ()),
        ] {
            let Err(TransactionError::Engine(Error::MergeFailed(key))) = result else {
                panic!("expected the merge failure, got {result:?}");
            };
            assert_eq!(key, b"k");
        }
    });
}

#[test]
fn a_merge_without_an_operator_is_refused_and_buffers_nothing() {
    each_flavour(Options::default, |flavour| {
        flavour.db().put(b"committed", b"v").unwrap();
        let tx = flavour.begin();
        for key in [&b"committed"[..], b"absent"] {
            assert!(
                matches!(
                    tx.merge(key, b"op"),
                    Err(TransactionError::Engine(Error::NoMergeOperator))
                ),
                "merge into {key:?}"
            );
        }
        tx.put(b"put", b"p").unwrap();
        assert_eq!(reads(&tx, b"committed").as_deref(), Some(&b"v"[..]));
        assert_eq!(reads(&tx, b"put").as_deref(), Some(&b"p"[..]));
        assert_eq!(reads(&tx, b"absent"), None);
        let scanned: Vec<_> = tx
            .scan_stream(None, None)
            .map(|item| {
                let (key, value) = item.unwrap();
                (key, value.to_vec())
            })
            .collect();
        assert_eq!(
            scanned,
            [
                (b"committed".to_vec(), b"v".to_vec()),
                (b"put".to_vec(), b"p".to_vec()),
            ]
        );
        tx.commit().unwrap();
        assert_eq!(flavour.db().get(b"absent").unwrap(), None);
        assert_eq!(flavour.db().get(b"committed").unwrap(), Some(b"v".to_vec()));
    });
}

/// `(key, value)` pairs of a transaction scan as counters.
fn scanned(
    tx: &Transaction,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    reverse: bool,
) -> Vec<(Vec<u8>, i64)> {
    let direction = if reverse {
        regolith::ScanDirection::Reverse
    } else {
        regolith::ScanDirection::Forward
    };
    let mut stream = tx.scan_stream_in(start, end, direction);
    stream
        .by_ref()
        .map(|item| {
            let (key, value) = item.unwrap();
            (key, counter(Some(value.to_vec())).unwrap())
        })
        .collect()
}

#[test]
fn a_scan_lays_the_merges_over_the_snapshot_in_both_directions() {
    each_flavour(counting, |flavour| {
        for (key, value) in [(b"a", 10), (b"c", 30), (b"d", 40), (b"e", 50)] {
            flavour.db().put(key, &delta(value)).unwrap();
        }
        let tx = flavour.begin();
        tx.merge(b"a", &delta(1)).unwrap();
        tx.merge(b"b", &delta(5)).unwrap();
        tx.merge(b"c", &delta(1)).unwrap();
        tx.put(b"c", &delta(100)).unwrap();
        tx.delete(b"d").unwrap();
        tx.merge(b"d", &delta(2)).unwrap();
        tx.merge(b"e", &delta(1)).unwrap();
        tx.delete(b"e").unwrap();
        tx.put(b"f", &delta(7)).unwrap();
        tx.merge(b"f", &delta(1)).unwrap();

        let want: Vec<(Vec<u8>, i64)> = [("a", 11), ("b", 5), ("c", 100), ("d", 2), ("f", 8)]
            .into_iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value))
            .collect();
        assert_eq!(scanned(&tx, None, None, false), want);

        let mut backwards = want.clone();
        backwards.reverse();
        assert_eq!(scanned(&tx, None, None, true), backwards);

        assert_eq!(
            scanned(&tx, Some(b"b"), Some(b"d"), false),
            want[1..3],
            "a bounded range holds only its keys"
        );
        assert_eq!(scanned(&tx, Some(b"b"), Some(b"d"), true), {
            let mut inner = want[1..3].to_vec();
            inner.reverse();
            inner
        });

        let first: Vec<_> = tx
            .scan_stream(None, None)
            .take(1)
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            first.len(),
            1,
            "a caller that stops early gets what it read"
        );
        assert_eq!(first[0].0, b"a".to_vec());
        assert_eq!(counter(Some(first[0].1.to_vec())), Some(11));
    });
}

#[test]
fn a_merged_key_reads_through_ten_thousand_unrelated_entries() {
    const UNRELATED: usize = 10_000;
    let unrelated = |i: usize| format!("u{i:05}").into_bytes();
    for options in BUFFER_SHAPES {
        each_flavour(options, |flavour| {
            flavour.db().put(b"above", &delta(10)).unwrap();
            flavour.db().put(b"below", &delta(20)).unwrap();
            let tx = flavour.begin();
            // `below` is merged before the unrelated entries and `above` after
            // them, so one chain lies beneath all of them and one over them.
            tx.merge(b"below", &delta(1)).unwrap();
            tx.merge(b"below", &delta(2)).unwrap();
            for i in 0..UNRELATED {
                tx.put(&unrelated(i), b"x").unwrap();
            }
            tx.merge(b"above", &delta(3)).unwrap();
            tx.merge(b"above", &delta(4)).unwrap();
            assert_eq!(counter(reads(&tx, b"below")), Some(23));
            assert_eq!(counter(reads(&tx, b"above")), Some(17));
            assert_eq!(reads(&tx, &unrelated(7)).as_deref(), Some(&b"x"[..]));

            // Another operand in each chain, and more entries in between.
            tx.merge(b"below", &delta(5)).unwrap();
            for i in UNRELATED..UNRELATED + 100 {
                tx.put(&unrelated(i), b"x").unwrap();
            }
            tx.merge(b"above", &delta(6)).unwrap();
            assert_eq!(counter(reads(&tx, b"below")), Some(28));
            assert_eq!(counter(reads(&tx, b"above")), Some(23));

            tx.commit().unwrap();
            assert_eq!(counter(flavour.db().get(b"below").unwrap()), Some(28));
            assert_eq!(counter(flavour.db().get(b"above").unwrap()), Some(23));
        });
    }
}

#[test]
fn threads_merging_into_one_key_of_a_shared_transaction_all_count() {
    const THREADS: i64 = 4;
    const MERGES: i64 = 250;
    for options in BUFFER_SHAPES {
        each_flavour(options, |flavour| {
            flavour.db().put(b"k", &delta(10)).unwrap();
            let tx = flavour.begin();
            // Past the default inline size before the threads start, so a
            // buffer that indexes itself has its index by then.
            for i in 0..64 {
                tx.put(format!("u{i:02}").as_bytes(), b"x").unwrap();
            }
            std::thread::scope(|scope| {
                for _ in 0..THREADS {
                    scope.spawn(|| {
                        for _ in 0..MERGES {
                            tx.merge(b"k", &delta(1)).unwrap();
                        }
                    });
                }
            });
            let total = 10 + THREADS * MERGES;
            assert_eq!(
                counter(reads(&tx, b"k")),
                Some(total),
                "a read sees every operand any thread made"
            );
            tx.commit().unwrap();
            assert_eq!(counter(flavour.db().get(b"k").unwrap()), Some(total));
        });
    }
}

#[test]
fn threads_putting_one_key_of_a_shared_transaction_read_what_they_commit() {
    const THREADS: usize = 4;
    const PUTS: usize = 200;
    for options in BUFFER_SHAPES {
        each_flavour(options, |flavour| {
            let tx = flavour.begin();
            for i in 0..64 {
                tx.put(format!("u{i:02}").as_bytes(), b"x").unwrap();
            }
            let shared = &tx;
            std::thread::scope(|scope| {
                for thread in 0..THREADS {
                    scope.spawn(move || {
                        for put in 0..PUTS {
                            shared
                                .put(b"k", format!("{thread}/{put}").as_bytes())
                                .unwrap();
                        }
                    });
                }
            });
            // Whichever put landed last, a read and the commit agree on it.
            let read = reads(&tx, b"k").expect("the key was put");
            tx.commit().unwrap();
            assert_eq!(flavour.db().get(b"k").unwrap(), Some(read));
        });
    }
}

#[test]
fn a_scan_that_reaches_a_merged_key_validates_it_as_a_read_at_every_level() {
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
        IsolationLevel::DefraLevel,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), counting()).unwrap();
        db.db().put(b"k", &delta(10)).unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(level));
        tx.merge(b"k", &delta(1)).unwrap();
        assert_eq!(
            scanned(&tx, None, None, false),
            [(b"k".to_vec(), 11)],
            "{level:?}"
        );
        db.db().merge(b"k", &delta(1)).unwrap();
        assert!(
            matches!(tx.commit(), Err(TransactionError::Conflict { .. })),
            "{level:?}: the scan walked the key the merge builds on"
        );
    }
}

/// `(key, value)` pairs of a transaction scan as text.
fn scanned_text(
    tx: &Transaction,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    reverse: bool,
) -> Vec<(String, String)> {
    let direction = if reverse {
        regolith::ScanDirection::Reverse
    } else {
        regolith::ScanDirection::Forward
    };
    let mut stream = tx.scan_stream_in(start, end, direction);
    stream
        .by_ref()
        .map(|item| {
            let (key, value) = item.unwrap();
            (
                String::from_utf8(key).unwrap(),
                String::from_utf8(value.to_vec()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_scan_applies_each_keys_operands_in_the_order_they_were_made() {
    each_flavour(appending, |flavour| {
        flavour.db().put(b"c", b"C").unwrap();
        let tx = flavour.begin();
        // Every key has a put or a committed value and two or three operands,
        // and the writes of the four keys are interleaved.
        tx.put(b"a", b"A").unwrap();
        tx.put(b"b", b"B").unwrap();
        tx.merge(b"a", b"1").unwrap();
        tx.merge(b"c", b"6").unwrap();
        tx.merge(b"b", b"4").unwrap();
        tx.merge(b"a", b"2").unwrap();
        tx.put(b"d", b"D").unwrap();
        tx.merge(b"c", b"7").unwrap();
        tx.merge(b"a", b"3").unwrap();
        tx.merge(b"b", b"5").unwrap();
        tx.merge(b"d", b"8").unwrap();
        tx.merge(b"c", b"9").unwrap();

        let rows = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        let all = [("a", "A123"), ("b", "B45"), ("c", "C679"), ("d", "D8")];
        let backwards: Vec<_> = all.iter().rev().copied().collect();

        assert_eq!(scanned_text(&tx, None, None, false), rows(&all));
        assert_eq!(scanned_text(&tx, None, None, true), rows(&backwards));
        assert_eq!(
            scanned_text(&tx, Some(b"b"), Some(b"d"), false),
            rows(&all[1..3])
        );
        assert_eq!(
            scanned_text(&tx, Some(b"b"), Some(b"d"), true),
            rows(&backwards[1..3])
        );
        assert_eq!(
            scanned_text(&tx, Some(b"c"), Some(b"d"), false),
            rows(&all[2..3]),
            "a range of one key"
        );
        tx.commit().unwrap();
        assert_eq!(
            flavour.db().get(b"c").unwrap().as_deref(),
            Some(&b"C679"[..])
        );
    });
}

#[test]
fn a_savepoint_rollback_restores_the_order_the_writes_were_made_in() {
    each_flavour(appending, |flavour| {
        let mut tx = flavour.begin();
        tx.put(b"k", b"P").unwrap();
        tx.merge(b"k", b"a").unwrap();
        tx.merge(b"k", b"b").unwrap();
        tx.set_savepoint();
        tx.merge(b"k", b"c").unwrap();
        assert_eq!(reads(&tx, b"k").as_deref(), Some(&b"Pabc"[..]));
        tx.rollback_to_savepoint().unwrap();
        assert_eq!(reads(&tx, b"k").as_deref(), Some(&b"Pab"[..]));
        tx.commit().unwrap();
        assert_eq!(
            flavour.db().get(b"k").unwrap().as_deref(),
            Some(&b"Pab"[..])
        );
    });
}

#[test]
fn a_savepoint_rollback_restores_the_merges_buffered_before_it() {
    each_flavour(counting, |flavour| {
        flavour.db().put(b"k", &delta(10)).unwrap();
        let mut tx = flavour.begin();
        tx.merge(b"k", &delta(1)).unwrap();
        tx.set_savepoint();
        tx.merge(b"k", &delta(2)).unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(13));
        tx.rollback_to_savepoint().unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(11));

        tx.set_savepoint();
        tx.put(b"k", &delta(100)).unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(100));
        tx.rollback_to_savepoint().unwrap();
        assert_eq!(
            counter(reads(&tx, b"k")),
            Some(11),
            "the merge buffered before the savepoint is back"
        );

        tx.put(b"k", &delta(50)).unwrap();
        tx.set_savepoint();
        tx.merge(b"k", &delta(1)).unwrap();
        tx.rollback_to_savepoint().unwrap();
        assert_eq!(counter(reads(&tx, b"k")), Some(50));
        tx.commit().unwrap();
        assert_eq!(counter(flavour.db().get(b"k").unwrap()), Some(50));
    });
}

#[test]
fn reading_a_merged_key_makes_the_merge_a_read_modify_write() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), counting()).unwrap();
    db.db().put(b"k", &delta(10)).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.merge(b"k", &delta(1)).unwrap();
    assert_eq!(counter(tx.get(b"k").unwrap()), Some(11));
    db.db().merge(b"k", &delta(1)).unwrap();
    assert!(
        matches!(tx.commit(), Err(TransactionError::Conflict { .. })),
        "the value the transaction read is stale"
    );

    let blind = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    blind.merge(b"k", &delta(1)).unwrap();
    db.db().merge(b"k", &delta(1)).unwrap();
    blind.commit().expect("a blind merge still commutes");
}
