//! `Transaction::get_parts` (plan 3.4): a read that names the parts of a value
//! its decision used is refused at commit only by a newer write that changes
//! one of them.
//!
//! Every case runs with the newer write in the memtable, flushed to a table,
//! and compacted, since the commit walks the key's versions in each source.
//! The merge operator is `common::parted`: values of three counters, and an
//! operand that touches exactly the part it names.

// Native-only: these use the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;

use common::parted::{self, Parted, add, parts_of, value};
use regolith::{
    Access, CommitReceipt, Db, Error, IsolationLevel, MergeOperator, OptimisticTransactionDb,
    Options, Transaction, TransactionDb, TransactionError, TxResult, TxnOptions, WriteKind,
};

const KEY: &[u8] = b"doc";

/// Where a newer write sits when the transaction commits.
#[derive(Clone, Copy, Debug)]
enum Layer {
    Memtable,
    Table,
    Compacted,
}

impl Layer {
    const ALL: [Self; 3] = [Self::Memtable, Self::Table, Self::Compacted];

    fn settle(self, db: &Db) {
        match self {
            Self::Memtable => {}
            Self::Table => db.flush().unwrap(),
            Self::Compacted => {
                db.flush().unwrap();
                db.compact_range(None, None).wait().unwrap();
            }
        }
    }
}

fn options() -> Options {
    Options::default().merge_operator(Some(Arc::new(Parted)))
}

fn defra() -> TxnOptions {
    TxnOptions::new().isolation(IsolationLevel::DefraLevel)
}

/// What a commit that lost reports, if it lost.
struct Lost {
    mine: Access,
    theirs: WriteKind,
    observed_seq: u64,
    latest_seq: u64,
}

fn lost(result: TxResult<CommitReceipt>) -> Option<Lost> {
    match result {
        Err(TransactionError::Conflict(c)) => {
            assert_eq!(c.key(), KEY, "the conflict names the key the read was of");
            Some(Lost {
                mine: c.mine(),
                theirs: c.theirs(),
                observed_seq: c.observed_seq(),
                latest_seq: c.latest_seq(),
            })
        }
        Ok(_) => None,
        Err(other) => panic!("expected a commit or a conflict, got {other:?}"),
    }
}

/// Seed `KEY` with `seed` (nothing when `None`), read it by `parts`, let
/// `concurrent` write around the transaction, settle it in `layer`, then let
/// `mine` add the transaction's own writes and commit. Every case is a
/// writing transaction: a write-free one validates nothing.
fn run(
    layer: Layer,
    seed: Option<[u64; 3]>,
    parts: &[u32],
    concurrent: impl FnOnce(&Db),
    mine: impl FnOnce(&Transaction),
) -> Option<Lost> {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    if let Some(seed) = seed {
        db.db().put(KEY, &value(seed)).unwrap();
    }
    let tx = db.begin(&defra());
    let got = tx.get_parts(KEY, parts).unwrap();
    assert_eq!(
        got.map(|bytes| parts_of(&bytes).unwrap()),
        seed,
        "get_parts returns the whole value"
    );
    concurrent(db.db());
    layer.settle(db.db());
    tx.put(b"elsewhere", b"x").unwrap();
    mine(&tx);
    lost(tx.commit())
}

const SEED: Option<[u64; 3]> = Some([10, 20, 30]);

fn merged(part: u32, delta: u64) -> impl FnOnce(&Db) {
    move |db| db.merge(KEY, &add(part, delta)).unwrap()
}

fn nothing(_: &Db) {}
fn leave(_: &Transaction) {}

#[test]
fn a_key_nothing_wrote_since_commits() {
    for layer in Layer::ALL {
        assert!(
            run(layer, SEED, &[0], nothing, leave).is_none(),
            "{layer:?}"
        );
    }
}

#[test]
fn an_operand_on_a_part_the_read_did_not_name_does_not_refuse_it() {
    for layer in Layer::ALL {
        for other in [1, 2] {
            let outcome = run(layer, SEED, &[0], merged(other, 5), leave);
            assert!(outcome.is_none(), "{layer:?} part {other}");
        }
        // Several operands, none of them on the named part.
        let outcome = run(
            layer,
            SEED,
            &[0],
            |db| {
                db.merge(KEY, &add(1, 1)).unwrap();
                db.merge(KEY, &add(2, 1)).unwrap();
                db.merge(KEY, &add(1, 1)).unwrap();
            },
            leave,
        );
        assert!(outcome.is_none(), "{layer:?}");
    }
}

#[test]
fn an_operand_on_a_named_part_refuses_it_with_the_reason_of_a_projected_read() {
    for layer in Layer::ALL {
        let lost =
            run(layer, SEED, &[0], merged(0, 1), leave).unwrap_or_else(|| panic!("{layer:?}"));
        assert_eq!(
            (lost.mine, lost.theirs),
            (Access::ReadParts, WriteKind::Merge)
        );
        assert!(lost.latest_seq > lost.observed_seq);

        // One of several named parts.
        let outcome = run(layer, SEED, &[0, 1], merged(1, 1), leave);
        assert!(outcome.is_some(), "{layer:?} one of two named parts");
    }
}

#[test]
fn the_walk_goes_past_operands_that_touch_nothing_named_to_the_one_that_does() {
    for layer in Layer::ALL {
        let touching_then_not = |db: &Db| {
            db.merge(KEY, &add(0, 1)).unwrap();
            db.merge(KEY, &add(2, 1)).unwrap();
            db.merge(KEY, &add(1, 1)).unwrap();
        };
        let lost =
            run(layer, SEED, &[0], touching_then_not, leave).unwrap_or_else(|| panic!("{layer:?}"));
        assert_eq!(lost.theirs, WriteKind::Merge);
        // The conflict names the operand that decided it, not the newest write.
        assert!(lost.latest_seq > lost.observed_seq);

        let not_then_touching = |db: &Db| {
            db.merge(KEY, &add(2, 1)).unwrap();
            db.merge(KEY, &add(0, 1)).unwrap();
        };
        assert!(
            run(layer, SEED, &[0], not_then_touching, leave).is_some(),
            "{layer:?}"
        );
    }
}

#[test]
fn the_reason_names_the_operand_that_touched_and_not_the_newest_write() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, &value([1, 2, 3])).unwrap();
    let tx = db.begin(&defra());
    tx.get_parts(KEY, &[0]).unwrap();
    db.db().merge(KEY, &add(0, 1)).unwrap();
    let touching = db.db().latest_sequence();
    db.db().merge(KEY, &add(2, 1)).unwrap();
    db.db().merge(KEY, &add(1, 1)).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    let lost = lost(tx.commit()).expect("conflict");
    assert_eq!(lost.latest_seq, touching);
}

#[test]
fn a_put_changes_every_part_unless_it_leaves_the_bytes_that_were_read() {
    for layer in Layer::ALL {
        let put = |bytes: Vec<u8>| move |db: &Db| db.put(KEY, &bytes).unwrap();
        let lost = run(layer, SEED, &[0], put(value([10, 20, 31])), leave)
            .unwrap_or_else(|| panic!("{layer:?}: part 2 changed but a put replaces every part"));
        assert_eq!(
            (lost.mine, lost.theirs),
            (Access::ReadParts, WriteKind::Put)
        );

        assert!(
            run(layer, SEED, &[0], put(value([10, 20, 30])), leave).is_none(),
            "{layer:?}: an identical rewrite is not a change"
        );
        // Changed and changed back: what is left is what was read.
        let there_and_back = |db: &Db| {
            db.put(KEY, &value([11, 20, 30])).unwrap();
            db.put(KEY, &value([10, 20, 30])).unwrap();
        };
        assert!(
            run(layer, SEED, &[0], there_and_back, leave).is_none(),
            "{layer:?}"
        );
    }
}

#[test]
fn operands_above_an_identical_put_count_only_when_they_touch_a_named_part() {
    for layer in Layer::ALL {
        let identical_then = |part: u32| {
            move |db: &Db| {
                db.put(KEY, &value([10, 20, 30])).unwrap();
                db.merge(KEY, &add(part, 1)).unwrap();
            }
        };
        // Compaction folds the operand into the put beneath it when no
        // snapshot lies between them, and the folded put no longer says which
        // part the operand touched. The read is then refused whatever part it
        // was on, which is safe and only conservative, so the cases that need
        // the operand to survive run before it.
        if !matches!(layer, Layer::Compacted) {
            let unrelated = run(layer, SEED, &[0], identical_then(1), leave);
            assert!(unrelated.is_none(), "{layer:?}");
        }
        let lost =
            run(layer, SEED, &[0], identical_then(0), leave).unwrap_or_else(|| panic!("{layer:?}"));
        let expected = match layer {
            Layer::Compacted => WriteKind::Put,
            Layer::Memtable | Layer::Table => WriteKind::Merge,
        };
        assert_eq!(lost.theirs, expected, "{layer:?}");

        // An older touching operand lies beneath the identical put, which
        // replaced it.
        let touching_then_identical = |db: &Db| {
            db.merge(KEY, &add(0, 1)).unwrap();
            db.put(KEY, &value([10, 20, 30])).unwrap();
        };
        assert!(
            run(layer, SEED, &[0], touching_then_identical, leave).is_none(),
            "{layer:?}"
        );
    }
}

#[test]
fn a_delete_changes_every_part() {
    for layer in Layer::ALL {
        let lost = run(layer, SEED, &[0], |db| db.delete(KEY).unwrap(), leave)
            .unwrap_or_else(|| panic!("{layer:?}"));
        assert_eq!(
            (lost.mine, lost.theirs),
            (Access::ReadParts, WriteKind::Delete)
        );

        let lost = run(
            layer,
            SEED,
            &[0],
            |db| db.delete_range(b"a", b"z").unwrap(),
            leave,
        )
        .unwrap_or_else(|| panic!("{layer:?}"));
        assert_eq!(lost.theirs, WriteKind::RangeDelete);

        // A range delete that does not reach the key changes nothing.
        let outcome = run(
            layer,
            SEED,
            &[0],
            |db| db.delete_range(b"a", b"b").unwrap(),
            leave,
        );
        assert!(outcome.is_none(), "{layer:?}");
    }
}

#[test]
fn a_read_that_found_nothing_is_validated_in_whole() {
    for layer in Layer::ALL {
        // Existence is part of every decision on parts: an operand on another
        // part creates the key.
        let lost =
            run(layer, None, &[0], merged(2, 1), leave).unwrap_or_else(|| panic!("{layer:?}"));
        assert_eq!(
            (lost.mine, lost.theirs),
            (Access::ReadParts, WriteKind::Merge)
        );
        assert!(
            run(
                layer,
                None,
                &[0],
                |db| db.put(KEY, &value([0; 3])).unwrap(),
                leave
            )
            .is_some(),
            "{layer:?}"
        );
        // A delete of a key that was never there leaves what was read.
        let outcome = run(layer, None, &[0], |db| db.delete(KEY).unwrap(), leave);
        assert!(outcome.is_none(), "{layer:?}");
    }
}

#[test]
fn a_key_the_transaction_puts_or_deletes_is_validated_in_whole() {
    for layer in Layer::ALL {
        // The operand is on a part the read did not name, which would pass
        // a projected read; the put replaces every part, so it must not.
        let lost = run(layer, SEED, &[0], merged(1, 1), |tx| {
            tx.put(KEY, &value([11, 0, 0])).unwrap();
        })
        .unwrap_or_else(|| panic!("{layer:?}: put"));
        assert_eq!(lost.mine, Access::ReadParts);

        let lost = run(layer, SEED, &[0], merged(1, 1), |tx| {
            tx.delete(KEY).unwrap()
        })
        .unwrap_or_else(|| panic!("{layer:?}: delete"));
        assert_eq!(lost.mine, Access::ReadParts);

        // An identical rewrite still is not a change: the put's own read is
        // current.
        let outcome = run(
            layer,
            SEED,
            &[0],
            |db| db.put(KEY, &value([10, 20, 30])).unwrap(),
            |tx| tx.put(KEY, &value([11, 20, 30])).unwrap(),
        );
        assert!(outcome.is_none(), "{layer:?}");
    }
}

#[test]
fn a_merge_into_a_key_read_by_parts_is_not_blind() {
    for layer in Layer::ALL {
        let mine = |tx: &Transaction| tx.merge(KEY, &add(1, 7)).unwrap();
        let outcome = run(layer, SEED, &[0], merged(2, 1), mine);
        assert!(outcome.is_none(), "{layer:?}: the named part is unchanged");
        let lost =
            run(layer, SEED, &[0], merged(0, 1), mine).unwrap_or_else(|| panic!("{layer:?}"));
        assert_eq!(
            lost.mine,
            Access::ReadParts,
            "the read decides, not the merge"
        );
    }
}

#[test]
fn a_read_by_no_parts_is_never_validated() {
    for layer in Layer::ALL {
        let writes: [fn(&Db); 3] = [
            |db| db.put(KEY, &value([99, 99, 99])).unwrap(),
            |db| db.delete(KEY).unwrap(),
            |db| db.merge(KEY, &add(0, 1)).unwrap(),
        ];
        for write in writes {
            assert!(
                run(layer, SEED, &[], write, leave).is_none(),
                "{layer:?}: found"
            );
            assert!(
                run(layer, None, &[], write, leave).is_none(),
                "{layer:?}: absent"
            );
        }
        // Not even when the transaction puts the key: that put is a blind
        // write, checked as one, and loses to a newer version of the key.
        let lost = run(
            layer,
            SEED,
            &[],
            |db| db.put(KEY, &value([99, 99, 99])).unwrap(),
            |tx| tx.put(KEY, &value([1, 1, 1])).unwrap(),
        )
        .unwrap_or_else(|| panic!("{layer:?}"));
        assert_eq!((lost.mine, lost.theirs), (Access::Put, WriteKind::Put));
    }
}

#[test]
fn the_parts_of_a_key_only_widen() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, &value([1, 2, 3])).unwrap();
    let outcome = |reads: &dyn Fn(&Transaction), write: u32| {
        let tx = db.begin(&defra());
        reads(&tx);
        db.db().merge(KEY, &add(write, 1)).unwrap();
        tx.put(b"elsewhere", b"x").unwrap();
        lost(tx.commit())
    };
    // Reading by one part, then another, names both.
    let both = |tx: &Transaction| {
        tx.get_parts(KEY, &[0]).unwrap();
        tx.get_parts(KEY, &[2]).unwrap();
    };
    assert!(outcome(&both, 0).is_some());
    assert!(outcome(&both, 2).is_some());
    assert!(outcome(&both, 1).is_none());
    // A read of the whole value, before or after, widens them to all.
    let then_whole = |tx: &Transaction| {
        tx.get_parts(KEY, &[0]).unwrap();
        tx.get(KEY).unwrap();
    };
    let lost = outcome(&then_whole, 1).expect("a whole read validates every part");
    assert_eq!(lost.mine, Access::Read);
    let whole_then = |tx: &Transaction| {
        tx.get(KEY).unwrap();
        tx.get_parts(KEY, &[0]).unwrap();
    };
    assert_eq!(outcome(&whole_then, 1).map(|l| l.mine), Some(Access::Read));
    let for_update = |tx: &Transaction| {
        tx.get_parts(KEY, &[0]).unwrap();
        tx.get_for_update(KEY).unwrap();
    };
    assert_eq!(
        outcome(&for_update, 1).map(|l| l.mine),
        Some(Access::ReadForUpdate)
    );
}

#[test]
fn a_read_of_the_transactions_own_write_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, &value([1, 2, 3])).unwrap();
    let tx = db.begin(&defra());
    tx.put(KEY, &value([9, 9, 9])).unwrap();
    let own = tx.get_parts(KEY, &[0]).unwrap().unwrap();
    assert_eq!(parts_of(&own), Some([9, 9, 9]));
    db.db().merge(KEY, &add(0, 1)).unwrap();
    // The put is the transaction's, a blind write over a newer version; the
    // read of it was never a read of the database.
    let lost = lost(tx.commit()).expect("the put loses to the operand");
    assert_eq!(lost.mine, Access::Put);
}

#[test]
fn below_defralevel_and_for_a_pessimistic_transaction_it_is_a_get() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, &value([1, 2, 3])).unwrap();
    for level in [IsolationLevel::RepeatableRead, IsolationLevel::Serializable] {
        let tx = db.begin(&TxnOptions::new().isolation(level));
        tx.get_parts(KEY, &[0]).unwrap();
        db.db().merge(KEY, &add(1, 1)).unwrap();
        tx.put(b"elsewhere", b"x").unwrap();
        let lost = lost(tx.commit()).unwrap_or_else(|| panic!("{level:?}"));
        assert_eq!(
            (lost.mine, lost.theirs),
            (Access::Read, WriteKind::Merge),
            "{level:?}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, &value([1, 2, 3])).unwrap();
    let tx = db.begin(&defra());
    tx.get_parts(KEY, &[0]).unwrap();
    db.db().merge(KEY, &add(1, 1)).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    let lost = lost(tx.commit()).expect("a pessimistic transaction validates as at RepeatableRead");
    assert_eq!(lost.mine, Access::Read);
}

/// An operator whose `touches` panics.
struct Panics;

impl MergeOperator for Panics {
    fn name(&self) -> &'static str {
        "panics"
    }

    fn full_merge(&self, key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        Parted.full_merge(key, base, operands)
    }

    fn touches(&self, _: &[u8], _: &[u8], _: &[u32]) -> bool {
        panic!("touches panicked")
    }
}

#[test]
fn a_panic_in_touches_fails_the_commit_and_latches_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default().merge_operator(Some(Arc::new(Panics))),
    )
    .unwrap();
    db.db().put(KEY, &value([1, 2, 3])).unwrap();
    let tx = db.begin(&defra());
    tx.get_parts(KEY, &[0]).unwrap();
    db.db().merge(KEY, &add(1, 1)).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    match tx.commit() {
        Err(TransactionError::Engine(Error::CallbackPanicked { callback, latched })) => {
            assert_eq!(callback, "MergeOperator");
            assert!(latched, "a merge operator panic latches the database");
        }
        other => panic!("expected the panic to fail the commit, got {other:?}"),
    }
    assert!(matches!(
        db.db().put(b"after", b"v"),
        Err(Error::CallbackPanicked { .. })
    ));
}

#[test]
fn the_operator_is_asked_only_when_a_newer_operand_exists() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(Arc<AtomicUsize>);

    impl MergeOperator for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn full_merge(&self, k: &[u8], b: Option<&[u8]>, o: &[&[u8]]) -> Option<Vec<u8>> {
            Parted.full_merge(k, b, o)
        }

        fn touches(&self, k: &[u8], operand: &[u8], parts: &[u32]) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            Parted.touches(k, operand, parts)
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default().merge_operator(Some(Arc::new(Counting(calls.clone())))),
    )
    .unwrap();
    db.db().put(KEY, &value([1, 2, 3])).unwrap();
    let tx = db.begin(&defra());
    tx.get_parts(KEY, &[0]).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    tx.commit().unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "nothing newer, nothing asked"
    );

    let tx = db.begin(&defra());
    tx.get_parts(KEY, &[0]).unwrap();
    for _ in 0..3 {
        db.db().merge(KEY, &add(1, 1)).unwrap();
    }
    tx.put(b"elsewhere", b"x").unwrap();
    tx.commit().unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        3,
        "one call per operand since the snapshot"
    );
}

#[test]
fn the_parted_operator_meets_the_touches_law() {
    // Guards the operator the cases above stand on.
    let folded = Parted.partial_merge(b"k", &add(1, 2), &add(1, 3)).unwrap();
    assert_eq!(folded, add(1, 5));
    for parts in [&[0u32][..], &[1], &[0, 1], &[2], &[]] {
        assert_eq!(
            Parted.touches(b"k", &folded, parts),
            Parted.touches(b"k", &add(1, 2), parts) || Parted.touches(b"k", &add(1, 3), parts)
        );
    }
    let _ = parted::PARTS;
}
