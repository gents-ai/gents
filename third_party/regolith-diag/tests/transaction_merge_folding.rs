//! Operand counts, not wall-clock deadlines, expose repeated merge work.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use regolith::{
    IsolationLevel, KeyClass, KeyClassifier, MergeOperator, OptimisticTransactionDb, Options,
    TxnOptions,
};

#[derive(Default)]
struct Sum {
    operands: AtomicUsize,
}

fn number(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

impl MergeOperator for Sum {
    fn name(&self) -> &'static str {
        "counted-sum"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        self.operands.fetch_add(operands.len(), Ordering::Relaxed);
        let mut value = base.map_or(Some(0), number)?;
        for operand in operands {
            value = value.wrapping_add(number(operand)?);
        }
        Some(value.to_be_bytes().to_vec())
    }

    fn partial_merge(&self, _: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        Some(
            number(left)?
                .wrapping_add(number(right)?)
                .to_be_bytes()
                .to_vec(),
        )
    }
}

#[test]
fn committed_batches_do_not_replay_every_original_operand() {
    const PAGES: usize = 32;
    const WRITES: usize = 64;
    let merge = Arc::new(Sum::default());
    let options = Options::memory()
        .write_buffer_size(64 * 1024 * 1024)
        .max_background_compactions(0)
        .inline_compaction(false)
        .merge_operator(Some(merge.clone()));
    let db = OptimisticTransactionDb::open("merge-folding", options).unwrap();
    for page in 1..=PAGES {
        let tx = db.begin(&TxnOptions::new());
        for _ in 0..WRITES {
            tx.merge(b"sum", &1u64.to_be_bytes()).unwrap();
        }
        tx.commit().unwrap();
        let reader = db.begin(&TxnOptions::new());
        assert_eq!(
            reader.get(b"sum").unwrap().as_deref(),
            Some(((page * WRITES) as u64).to_be_bytes().as_slice())
        );
    }
    let operands = merge.operands.load(Ordering::Relaxed);
    assert_eq!(
        operands,
        PAGES * (PAGES + 1) / 2,
        "{PAGES} batches of {WRITES} writes folded {operands} operands on reads"
    );
}

#[test]
fn folding_keeps_snapshots_replacements_and_savepoints_at_every_level() {
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
        IsolationLevel::DefraLevel,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let options = || Options::default().merge_operator(Some(Arc::new(Sum::default())));
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        db.db().put(b"sum", &10u64.to_be_bytes()).unwrap();
        let old = db.db().snapshot();
        let mut writer = db.begin(&TxnOptions::new().isolation(level));
        for _ in 0..8 {
            writer.merge(b"sum", &1u64.to_be_bytes()).unwrap();
        }
        writer.set_savepoint();
        writer.merge(b"sum", &100u64.to_be_bytes()).unwrap();
        writer.rollback_to_savepoint().unwrap();
        for _ in 8..64 {
            writer.merge(b"sum", &1u64.to_be_bytes()).unwrap();
            writer.merge(b"other", &100u64.to_be_bytes()).unwrap();
        }
        writer.put(b"other", &5u64.to_be_bytes()).unwrap();
        writer.merge(b"other", &3u64.to_be_bytes()).unwrap();
        writer.delete(b"other").unwrap();
        writer.merge(b"other", &2u64.to_be_bytes()).unwrap();
        writer.merge(b"other", &3u64.to_be_bytes()).unwrap();
        assert_eq!(
            writer.get(b"sum").unwrap(),
            Some(74u64.to_be_bytes().to_vec())
        );
        assert_eq!(
            writer.get(b"other").unwrap(),
            Some(5u64.to_be_bytes().to_vec())
        );
        writer.commit().unwrap();
        assert_eq!(old.get(b"sum").unwrap(), Some(10u64.to_be_bytes().to_vec()));
        db.db().flush().unwrap();
        db.db().compact_range(None, None).wait().unwrap();
        assert_eq!(old.get(b"sum").unwrap(), Some(10u64.to_be_bytes().to_vec()));
        drop(old);
        drop(db);
        let reopened = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        assert_eq!(
            reopened.db().get(b"sum").unwrap(),
            Some(74u64.to_be_bytes().to_vec())
        );
        assert_eq!(
            reopened.db().get(b"other").unwrap(),
            Some(5u64.to_be_bytes().to_vec())
        );
    }
}

struct Floating;

impl MergeOperator for Floating {
    fn name(&self) -> &'static str {
        "ordered-float"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let decode = |bytes: &[u8]| Some(f64::from_be_bytes(bytes.try_into().ok()?));
        let mut value = base.map_or(Some(0.0), decode)?;
        for operand in operands {
            value += decode(operand)?;
        }
        Some(value.to_be_bytes().to_vec())
    }
}

#[test]
fn declined_folds_keep_non_associative_operand_order() {
    let options = Options::memory().merge_operator(Some(Arc::new(Floating)));
    let db = OptimisticTransactionDb::open("float-folding", options).unwrap();
    let tx = db.begin(&TxnOptions::new());
    for value in [1e20_f64, -1e20, 1.0] {
        tx.merge(b"sum", &value.to_be_bytes()).unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(
        db.db().get(b"sum").unwrap(),
        Some(1.0_f64.to_be_bytes().to_vec())
    );
}

#[derive(Default)]
struct Append {
    calls: AtomicUsize,
}

impl MergeOperator for Append {
    fn name(&self) -> &'static str {
        "growing-append"
    }

    fn full_merge(&self, key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        assert_eq!(key, b"list");
        let mut value = base.unwrap_or_default().to_vec();
        for operand in operands {
            value.extend_from_slice(operand);
        }
        Some(value)
    }

    fn partial_merge(&self, key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        assert_eq!(key, b"list");
        self.calls.fetch_add(1, Ordering::Relaxed);
        Some([left, right].concat())
    }
}

#[test]
fn growing_folds_leave_valid_operands_uncombined() {
    let merge = Arc::new(Append::default());
    let options = Options::memory().merge_operator(Some(merge.clone()));
    let db = OptimisticTransactionDb::open("growing-folding", options).unwrap();
    let tx = db.begin(&TxnOptions::new());
    for byte in 0..64 {
        tx.merge(b"list", &[byte]).unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(merge.calls.load(Ordering::Relaxed), 32);
    assert_eq!(db.db().get(b"list").unwrap(), Some((0..64).collect()));
}

struct Pair;

impl MergeOperator for Pair {
    fn name(&self) -> &'static str {
        "part-pair"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut value: [u8; 2] = base.unwrap_or(&[0, 0]).try_into().ok()?;
        for operand in operands {
            let [_, a, b]: [u8; 3] = (*operand).try_into().ok()?;
            value[0] = value[0].wrapping_add(a);
            value[1] = value[1].wrapping_add(b);
        }
        Some(value.to_vec())
    }

    fn partial_merge(&self, _: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let [mask_a, a, b]: [u8; 3] = left.try_into().ok()?;
        let [mask_b, c, d]: [u8; 3] = right.try_into().ok()?;
        Some(vec![mask_a | mask_b, a.wrapping_add(c), b.wrapping_add(d)])
    }

    fn touches(&self, _: &[u8], operand: &[u8], parts: &[u32]) -> bool {
        let Some(mask) = operand.first() else {
            return true;
        };
        parts
            .iter()
            .any(|part| *part < 2 && mask & (1 << part) != 0)
    }
}

struct Ordinary;

impl KeyClassifier for Ordinary {
    fn classify(&self, _: &[u8]) -> KeyClass {
        KeyClass::Ordinary
    }
}

#[test]
fn folded_operands_keep_projected_conflicts() {
    for touched in [false, true] {
        let options = Options::memory().merge_operator(Some(Arc::new(Pair)));
        let db = OptimisticTransactionDb::open("parts-folding", options)
            .unwrap()
            .with_policy(Arc::new(Ordinary));
        db.db().put(b"pair", &[0, 0]).unwrap();
        let opts = TxnOptions::new().isolation(IsolationLevel::DefraLevel);
        let reader = db.begin(&opts);
        assert_eq!(
            reader.get_parts(b"pair", &[0]).unwrap().unwrap().as_slice(),
            &[0, 0]
        );
        reader.put(b"decision", b"made").unwrap();
        let writer = db.begin(&opts);
        writer
            .merge(b"pair", if touched { &[1, 1, 0] } else { &[2, 0, 1] })
            .unwrap();
        writer.merge(b"pair", &[2, 0, 1]).unwrap();
        writer.commit().unwrap();
        assert_eq!(reader.commit().is_err(), touched);
    }
}

struct CompactSum;

impl MergeOperator for CompactSum {
    fn name(&self) -> &'static str {
        "compact-sum"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let value = base
            .unwrap_or_default()
            .iter()
            .chain(operands.iter().flat_map(|bytes| bytes.iter()))
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        Some(vec![value])
    }

    fn partial_merge(&self, key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        self.full_merge(key, None, &[left, right])
    }
}

#[test]
fn shrinking_an_operand_cannot_hide_an_oversized_input() {
    for oversized in [false, true] {
        let options = Options::memory()
            .max_value_size(2)
            .merge_operator(Some(Arc::new(CompactSum)));
        let db = OptimisticTransactionDb::open("size-folding", options).unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.merge(b"sum", if oversized { &[0, 0, 1] } else { &[0, 1] })
            .unwrap();
        tx.merge(b"sum", &[0, 1]).unwrap();
        assert_eq!(tx.commit().is_err(), oversized);
        assert_eq!(
            db.db().get(b"sum").unwrap(),
            (!oversized).then_some(vec![2])
        );
    }
}

struct FramedAppend;

impl MergeOperator for FramedAppend {
    fn name(&self) -> &'static str {
        "framed-append"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut value = base.unwrap_or_default().to_vec();
        for operand in operands {
            value.extend_from_slice(operand.get(1..)?);
        }
        Some(value)
    }

    fn partial_merge(&self, _: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        Some([&[0][..], left.get(1..)?, right.get(1..)?].concat())
    }
}

#[test]
fn a_smaller_combined_payload_can_still_exceed_the_value_limit() {
    for limit in [3, 5] {
        let options = Options::memory()
            .max_value_size(limit)
            .merge_operator(Some(Arc::new(FramedAppend)));
        let db = OptimisticTransactionDb::open("combined-size-folding", options).unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.merge(b"list", &[0, 1, 2]).unwrap();
        tx.merge(b"list", &[0, 3, 4]).unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"list").unwrap(), Some(vec![1, 2, 3, 4]));
    }
}

#[derive(Default)]
struct Growing {
    bytes: AtomicUsize,
    calls: AtomicUsize,
}

impl MergeOperator for Growing {
    fn name(&self) -> &'static str {
        "growing"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut result = base.unwrap_or_default().to_vec();
        for operand in operands {
            result.extend_from_slice(operand.strip_prefix(b"operand:")?);
        }
        Some(result)
    }

    fn partial_merge(&self, _: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.bytes
            .fetch_add(left.len() + right.len(), Ordering::Relaxed);
        let mut result = left.to_vec();
        result.extend_from_slice(right.strip_prefix(b"operand:")?);
        Some(result)
    }
}

#[test]
fn growing_operands_have_logarithmically_bounded_commit_work() {
    const COUNT: u32 = 10_000;
    let op = Arc::new(Growing::default());
    let db = OptimisticTransactionDb::open(
        "growing-merge-work",
        Options::memory().merge_operator(Some(op.clone())),
    )
    .unwrap();
    let tx = db.begin(&TxnOptions::new());
    let mut expected = Vec::new();
    for index in 0..COUNT {
        let mut operand = b"operand:".to_vec();
        operand.extend_from_slice(&index.to_be_bytes());
        tx.merge(b"key", &operand).unwrap();
        expected.extend_from_slice(&index.to_be_bytes());
    }
    tx.commit().unwrap();
    let bytes = op.bytes.load(Ordering::Relaxed);
    let levels = COUNT.next_power_of_two().ilog2() as usize;
    assert!(
        bytes <= COUNT as usize * 12 * levels,
        "fold processed {bytes} bytes"
    );
    assert_eq!(op.calls.load(Ordering::Relaxed), COUNT as usize - 1);
    assert_eq!(db.db().get(b"key").unwrap().unwrap(), expected);
}
