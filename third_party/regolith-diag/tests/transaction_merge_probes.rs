//! A key merged N times in one optimistic transaction is validated with
//! one conflict probe, not N.
//!
//! A commit probes its keys up to the read horizon before it queues, and the
//! commit group's leader then looks only at the memtables for what landed
//! above it, so a probe per merge op instead of per distinct key multiplies
//! commit cost by the merges in the transaction. A key only reaches a table
//! when something committed after the transaction's snapshot: with nothing
//! newer, no key can conflict and none is probed at all.
//! A probe stops at the key's newest entry, merge operands included: a
//! newer operand is a newer write, wherever it is stored. `Statistics::BloomFilterFullPositive`
//! is the public-API proxy for probe count here: each probe that reaches
//! the flushed SSTable and finds the key bumps it by exactly one.
//!
//! The commit sorts a transaction's merges by key, which is what lets it
//! skip a repeat of the previous key. The sort must be stable: each key's
//! operands still read back in the order they were buffered.

use std::sync::Arc;

use regolith::{
    IsolationLevel, MergeOperator, OptimisticTransactionDb, Options, Statistics, Ticker,
    TransactionError, TxnOptions,
};

/// Sums big-endian i64 deltas, so two `+1` operands make `+2` and the
/// operation is plainly not idempotent.
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

#[test]
fn a_key_merged_many_times_is_probed_once() {
    const MERGES: i64 = 100;

    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let options = Options::default()
        .merge_operator(Some(Arc::new(CounterMerge)))
        .statistics(Some(stats.clone()));
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();

    // Flush the base value so every probe below has to reach the SSTable
    // and consult its bloom filter, rather than being served from the
    // active memtable where no bloom check happens.
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();
    db.db().flush().unwrap();
    stats.reset();

    // Nothing committed since the snapshot: no key can have changed, and
    // none is looked up in a table.
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    for _ in 0..MERGES {
        tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(
        stats.get_ticker(Ticker::BloomFilterFullPositive),
        0,
        "with nothing committed since the snapshot, no key is probed in a table"
    );

    // A write after the snapshot makes the commit look, once per key, and
    // the flush puts the key's newest version back in a table.
    db.db().flush().unwrap();
    stats.reset();
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    db.db().put(b"unrelated", b"newer").unwrap();
    db.db().flush().unwrap();
    for _ in 0..MERGES {
        tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    }
    tx.commit().unwrap();

    assert_eq!(
        stats.get_ticker(Ticker::BloomFilterFullPositive),
        1,
        "a key merged {MERGES} times must be probed once, not once per merge op"
    );
    assert_eq!(
        db.db().get(b"counter").unwrap().as_deref(),
        Some((2 * MERGES).to_be_bytes().as_slice())
    );
}

#[test]
fn keys_merged_in_an_interleaved_order_are_probed_once_each() {
    const KEYS: [&[u8]; 3] = [b"a", b"b", b"c"];
    const MERGES: usize = 30;

    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let options = Options::default()
        .merge_operator(Some(Arc::new(CounterMerge)))
        .statistics(Some(stats.clone()));
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();

    // Flushed, as above, so every probe has to consult the SSTable's bloom
    // filter.
    for key in KEYS {
        db.db().put(key, &0i64.to_be_bytes()).unwrap();
    }
    db.db().flush().unwrap();
    stats.reset();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    // A write after the snapshot, so the commit has to look.
    db.db().put(b"unrelated", b"newer").unwrap();
    db.db().flush().unwrap();
    for i in 0..MERGES {
        tx.merge(KEYS[i % KEYS.len()], &1i64.to_be_bytes()).unwrap();
    }
    tx.commit().unwrap();

    assert_eq!(
        stats.get_ticker(Ticker::BloomFilterFullPositive),
        KEYS.len() as u64,
        "{MERGES} merges interleaved over {} keys must be probed once per key",
        KEYS.len()
    );
    for key in KEYS {
        assert_eq!(
            db.db().get(key).unwrap().as_deref(),
            Some(((MERGES / KEYS.len()) as i64).to_be_bytes().as_slice())
        );
    }
}

/// Appends every operand to the base in the order a read folds them, so a
/// change to the order operands were buffered in shows in the result.
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

#[test]
fn operands_merged_in_an_interleaved_order_read_back_in_the_order_they_were_buffered() {
    const KEYS: [&[u8]; 3] = [b"a", b"b", b"c"];
    const OPERANDS: usize = 90;

    let dir = tempfile::tempdir().unwrap();
    let options = Options::default().merge_operator(Some(Arc::new(AppendMerge)));
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    let mut expected: [Vec<u8>; KEYS.len()] = Default::default();
    for i in 0..OPERANDS {
        // An uneven walk over the keys, so no key's operands sit side by side.
        let key = (i * 7 + i / 3) % KEYS.len();
        let operand = format!("<{i:03}>").into_bytes();
        tx.merge(KEYS[key], &operand).unwrap();
        expected[key].extend_from_slice(&operand);
    }
    tx.commit().unwrap();

    for (key, expected) in KEYS.into_iter().zip(&expected) {
        assert!(!expected.is_empty(), "key {key:?} was never merged into");
        assert_eq!(
            db.db().get(key).unwrap().as_deref(),
            Some(expected.as_slice()),
            "operands of key {key:?} must read back in the order they were buffered"
        );
    }
}

/// Where the newer merge operand sits when the reader commits.
#[derive(Clone, Copy, Debug)]
enum OperandAt {
    Memtable,
    /// Flushed into its own L0 file, above the flushed base.
    SeparateTable,
    /// Compacted into the same table as the base it sits on.
    SameTable,
}

/// A point read is stale once a newer merge operand commits on its key,
/// wherever that operand is stored. The conflict check used to see only
/// the memtable copy: the SSTable probe walked past merge operands to
/// the base under them, so after a flush the reader's anchor looked
/// current and a stale read committed.
#[test]
fn a_newer_merge_operand_conflicts_a_point_read_wherever_it_is_stored() {
    for at in [
        OperandAt::Memtable,
        OperandAt::SeparateTable,
        OperandAt::SameTable,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let options = Options::default().merge_operator(Some(Arc::new(CounterMerge)));
        let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();
        db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();
        db.db().flush().unwrap();

        let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
        assert_eq!(
            tx.get(b"counter").unwrap().as_deref(),
            Some(0i64.to_be_bytes().as_slice())
        );

        db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
        match at {
            OperandAt::Memtable => {}
            OperandAt::SeparateTable => db.db().flush().unwrap(),
            OperandAt::SameTable => {
                db.db().flush().unwrap();
                db.db().compact_range(None, None).wait().unwrap();
            }
        }

        tx.put(b"elsewhere", b"x").unwrap();
        assert!(
            matches!(tx.commit(), Err(TransactionError::Conflict { .. })),
            "a read of `counter` overtaken by a merge operand stored in {at:?} must conflict"
        );
    }
}
