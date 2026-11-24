//! Compaction cuts a key's versions into stripes at the live snapshots, and
//! drops, folds and filters inside a stripe without crossing its boundary.
//!
//! Under steady transactional load a snapshot is live at every instant. A
//! compaction that waited for none would never fold a hot key's merge chain
//! nor run a compaction filter. The model test is the one that has to hold
//! for stripes to be safe: it checks what every reader sees, at every
//! snapshot and at the head, against a model of the writes, across flushes
//! and compactions of every kind. The tests after it pin what stripes buy.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem and threads. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use regolith::{
    CompactionDecision, CompactionFilter, CompactionOutcome, CompactionStyle, CompressionType, Db,
    DbWithTtl, Env, IsolationLevel, MemEnv, MergeOperator, OptimisticTransactionDb, Options,
    Snapshot, TransactionError, TxResult, TxnOptions,
};

/// Sums big-endian `i64` deltas. `fold` is whether two deltas fold into one
/// without a base value.
struct Sum {
    fold: bool,
}

impl Sum {
    /// The delta or total `bytes` encode, if they encode one.
    fn decode(bytes: &[u8]) -> Option<i64> {
        Some(i64::from_be_bytes(bytes.try_into().ok()?))
    }
}

impl MergeOperator for Sum {
    fn name(&self) -> &'static str {
        "sum"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut total = base.map_or(Some(0), Self::decode)?;
        for operand in operands {
            total = total.wrapping_add(Self::decode(operand)?);
        }
        Some(total.to_be_bytes().to_vec())
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        if !self.fold {
            return None;
        }
        let sum = Self::decode(left)?.wrapping_add(Self::decode(right)?);
        Some(sum.to_be_bytes().to_vec())
    }
}

/// Concatenates, oldest first. Associative and not commutative, so a fold
/// in the wrong order shows in the bytes.
struct Append;

impl MergeOperator for Append {
    fn name(&self) -> &'static str {
        "append"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut merged = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands.iter().for_each(|operand| merged.extend(*operand));
        Some(merged)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        Some([left, right].concat())
    }
}

// --- the model ----------------------------------------------------------

const KEYS: [&[u8]; 4] = [b"a", b"b", b"c", b"d"];

/// Range bounds: the keys, and one past the last.
const BOUNDS: [&[u8]; 5] = [b"a", b"b", b"c", b"d", b"e"];

/// The most snapshots held at once.
const MAX_HELD: usize = 6;

/// Which operator, and so which value encoding, a case runs against.
#[derive(Clone, Copy, Debug)]
enum Flavor {
    SumFolding,
    SumUnfolding,
    Append,
}

impl Flavor {
    /// The operator this flavor runs.
    fn operator(self) -> Arc<dyn MergeOperator> {
        match self {
            Self::SumFolding => Arc::new(Sum { fold: true }),
            Self::SumUnfolding => Arc::new(Sum { fold: false }),
            Self::Append => Arc::new(Append),
        }
    }

    /// A value or operand: `n` for a sum, a tag unique to the write for an
    /// append, so that any reordering of operands changes the bytes.
    fn bytes(self, n: u8, serial: usize) -> Vec<u8> {
        match self {
            Self::SumFolding | Self::SumUnfolding => i64::from(n).to_be_bytes().to_vec(),
            Self::Append => format!("{}{serial};", char::from(b'a' + n)).into_bytes(),
        }
    }

    /// What `operand` merged onto `state` leaves. The model's own account of
    /// merging, written apart from the operators above.
    fn apply(self, state: Option<&[u8]>, operand: &[u8]) -> Vec<u8> {
        match self {
            Self::SumFolding | Self::SumUnfolding => {
                let int = |bytes: &[u8]| i64::from_be_bytes(bytes.try_into().unwrap());
                let base = state.map_or(0, int);
                base.wrapping_add(int(operand)).to_be_bytes().to_vec()
            }
            Self::Append => [state.unwrap_or_default(), operand].concat(),
        }
    }
}

/// One write the model replays, at the sequence the database gave it.
#[derive(Clone, Debug)]
enum Write {
    Put(usize, Vec<u8>),
    Delete(usize),
    Merge(usize, Vec<u8>),
    DeleteRange(usize, usize),
}

/// What a reader at sequence `at` makes of key `key`.
fn model_get(flavor: Flavor, log: &[(u64, Write)], key: usize, at: u64) -> Option<Vec<u8>> {
    let mut state: Option<Vec<u8>> = None;
    for (_, write) in log.iter().take_while(|(seq, _)| *seq <= at) {
        match write {
            Write::Put(k, value) if *k == key => state = Some(value.clone()),
            Write::Delete(k) if *k == key => state = None,
            Write::Merge(k, operand) if *k == key => {
                state = Some(flavor.apply(state.as_deref(), operand));
            }
            Write::DeleteRange(lo, hi) if (*lo..*hi).contains(&key) => state = None,
            _ => {}
        }
    }
    state
}

/// What a full scan at sequence `at` makes of the whole keyspace.
fn model_scan(flavor: Flavor, log: &[(u64, Write)], at: u64) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..KEYS.len())
        .filter_map(|key| Some((KEYS[key].to_vec(), model_get(flavor, log, key, at)?)))
        .collect()
}

/// A step of a generated case.
///
/// `compact_range` is only ever asked for the whole keyspace. Over a partial
/// range it can push a newer level-0 file below an older one that overlaps
/// it, so the older file's value for a key outside the range outranks the
/// newer one: a recency inversion that happens with or without snapshots,
/// and one this model would report for a reason that has nothing to do with
/// stripes.
#[derive(Clone, Debug)]
enum Op {
    Put(usize, u8),
    Delete(usize),
    Merge(usize, u8),
    DeleteRange(usize, usize),
    /// Take a snapshot and hold it.
    Snapshot,
    /// Release the held snapshot at this index, modulo how many are held.
    Release(usize),
    Flush,
    Compact,
    /// Drain `compact_step` until it reports idle.
    CompactStep,
}

/// A step of a case, weighted toward writes that leave operands to fold.
fn op() -> impl Strategy<Value = Op> {
    let key = 0..KEYS.len();
    let span = || (0..KEYS.len()).prop_flat_map(|lo| (Just(lo), lo + 1..=KEYS.len()));
    prop_oneof![
        3 => (key.clone(), 0u8..8).prop_map(|(k, n)| Op::Put(k, n)),
        2 => key.clone().prop_map(Op::Delete),
        6 => (key, 0u8..8).prop_map(|(k, n)| Op::Merge(k, n)),
        1 => span().prop_map(|(lo, hi)| Op::DeleteRange(lo, hi)),
        3 => Just(Op::Snapshot),
        2 => (0usize..MAX_HELD).prop_map(Op::Release),
        2 => Just(Op::Flush),
        2 => Just(Op::Compact),
        2 => Just(Op::CompactStep),
    ]
}

/// The compaction styles that merge: leveled and universal.
fn style() -> impl Strategy<Value = CompactionStyle> {
    prop_oneof![
        Just(CompactionStyle::Level),
        Just(CompactionStyle::Universal)
    ]
}

/// Every reader reads every key, and scans the keyspace, as the model says.
fn check(
    flavor: Flavor,
    db: &Db,
    log: &[(u64, Write)],
    held: &[Snapshot],
) -> Result<(), TestCaseError> {
    let head = db.latest_sequence();
    for (key, name) in KEYS.iter().enumerate() {
        prop_assert_eq!(
            db.get(name).unwrap(),
            model_get(flavor, log, key, head),
            "head, key {:?}",
            name
        );
    }
    prop_assert_eq!(
        db.scan(None, None).unwrap(),
        model_scan(flavor, log, head),
        "head scan"
    );
    for snapshot in held {
        let at = snapshot.sequence();
        for (key, name) in KEYS.iter().enumerate() {
            prop_assert_eq!(
                snapshot.get(name).unwrap(),
                model_get(flavor, log, key, at),
                "snapshot at {}, key {:?}",
                at,
                name
            );
        }
        prop_assert_eq!(
            snapshot.scan(None, None).unwrap(),
            model_scan(flavor, log, at),
            "scan at snapshot {}",
            at
        );
    }
    Ok(())
}

/// Run `ops` against a database and the model together.
fn run(flavor: Flavor, style: CompactionStyle, ops: &[Op]) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .merge_operator(Some(flavor.operator()))
            .compaction_style(style)
            // Compaction runs only where the case asks for it.
            .max_background_compactions(0)
            .l0_compaction_trigger(2)
            // One key's versions per output file, so a compaction splits the
            // tree into many files and later ones pick among them.
            .target_file_size(1),
    )
    .unwrap();

    let mut log: Vec<(u64, Write)> = Vec::new();
    let mut held: Vec<Snapshot> = Vec::new();
    for (serial, op) in ops.iter().enumerate() {
        let write = match *op {
            Op::Put(key, n) => {
                let value = flavor.bytes(n, serial);
                db.put(KEYS[key], &value).unwrap();
                Some(Write::Put(key, value))
            }
            Op::Delete(key) => {
                db.delete(KEYS[key]).unwrap();
                Some(Write::Delete(key))
            }
            Op::Merge(key, n) => {
                let operand = flavor.bytes(n, serial);
                db.merge(KEYS[key], &operand).unwrap();
                Some(Write::Merge(key, operand))
            }
            Op::DeleteRange(lo, hi) => {
                db.delete_range(BOUNDS[lo], BOUNDS[hi]).unwrap();
                Some(Write::DeleteRange(lo, hi))
            }
            Op::Snapshot => {
                if held.len() < MAX_HELD {
                    held.push(db.snapshot());
                }
                None
            }
            Op::Release(index) => {
                if !held.is_empty() {
                    held.remove(index % held.len());
                }
                None
            }
            Op::Flush => {
                db.flush().unwrap();
                check(flavor, &db, &log, &held)?;
                None
            }
            Op::Compact => {
                db.compact_range(None, None).wait().unwrap();
                check(flavor, &db, &log, &held)?;
                None
            }
            Op::CompactStep => {
                for _ in 0..64 {
                    if db.compact_step().unwrap() == CompactionOutcome::Idle {
                        break;
                    }
                }
                check(flavor, &db, &log, &held)?;
                None
            }
        };
        if let Some(write) = write {
            log.push((db.latest_sequence(), write));
        }
    }
    check(flavor, &db, &log, &held)
}

proptest! {
    // Each case opens a database, so far fewer than the default 256.
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn sums_that_fold_read_as_the_model_does(
        style in style(),
        ops in proptest::collection::vec(op(), 0..64),
    ) {
        run(Flavor::SumFolding, style, &ops)?;
    }

    #[test]
    fn sums_that_cannot_fold_read_as_the_model_does(
        style in style(),
        ops in proptest::collection::vec(op(), 0..64),
    ) {
        run(Flavor::SumUnfolding, style, &ops)?;
    }

    #[test]
    fn appends_keep_their_order(
        style in style(),
        ops in proptest::collection::vec(op(), 0..64),
    ) {
        run(Flavor::Append, style, &ops)?;
    }
}

// --- what stripes buy ---------------------------------------------------

const OPERANDS: i64 = 1_000;

/// Bytes of SSTable on disk.
fn sst_bytes(db: &Db) -> u64 {
    db.get_int_property("regolith.total-sst-files-size")
        .unwrap()
}

/// The `i64` a value encodes.
fn int(bytes: Vec<u8>) -> i64 {
    i64::from_be_bytes(bytes[..].try_into().unwrap())
}

/// The counter at the head.
fn counter(db: &Db) -> i64 {
    int(db.get(b"counter").unwrap().unwrap())
}

/// Options for a counter database; `fold` is whether two deltas fold alone.
fn counter_options(fold: bool) -> Options {
    Options::default()
        .merge_operator(Some(Arc::new(Sum { fold })))
        // Raw bytes, so file sizes follow the entry count.
        .compression(CompressionType::None)
        .max_background_compactions(0)
}

/// Merge [`OPERANDS`] increments of one into the counter.
fn merge_ones(db: &Db) {
    for _ in 0..OPERANDS {
        db.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    }
}

#[test]
fn operands_above_a_live_snapshot_fold_when_the_operator_can() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), counter_options(true)).unwrap();
    db.put(b"counter", &100i64.to_be_bytes()).unwrap();
    let snapshot = db.snapshot();
    merge_ones(&db);
    db.flush().unwrap();
    let flushed = sst_bytes(&db);

    db.compact_range(None, None).wait().unwrap();

    assert!(
        sst_bytes(&db) < flushed / 4,
        "the chain was folded: {} bytes from {flushed}",
        sst_bytes(&db)
    );
    assert_eq!(counter(&db), 100 + OPERANDS);
    assert_eq!(int(snapshot.get(b"counter").unwrap().unwrap()), 100);
}

#[test]
fn operands_above_a_live_snapshot_stay_operands_when_the_operator_cannot_fold_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), counter_options(false)).unwrap();
    db.put(b"counter", &100i64.to_be_bytes()).unwrap();
    let snapshot = db.snapshot();
    merge_ones(&db);
    db.flush().unwrap();
    let flushed = sst_bytes(&db);

    db.compact_range(None, None).wait().unwrap();

    assert!(
        sst_bytes(&db) > flushed / 2,
        "the chain stayed whole: {} bytes from {flushed}",
        sst_bytes(&db)
    );
    assert_eq!(counter(&db), 100 + OPERANDS);
    assert_eq!(int(snapshot.get(b"counter").unwrap().unwrap()), 100);
}

#[test]
fn a_put_above_a_live_snapshot_folds_the_operands_over_it_whatever_the_operator() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), counter_options(false)).unwrap();
    db.put(b"counter", &100i64.to_be_bytes()).unwrap();
    let snapshot = db.snapshot();
    db.put(b"counter", &500i64.to_be_bytes()).unwrap();
    merge_ones(&db);
    db.flush().unwrap();
    let flushed = sst_bytes(&db);

    db.compact_range(None, None).wait().unwrap();

    assert!(
        sst_bytes(&db) < flushed / 4,
        "the chain was folded onto the put: {} bytes from {flushed}",
        sst_bytes(&db)
    );
    assert_eq!(counter(&db), 500 + OPERANDS);
    assert_eq!(int(snapshot.get(b"counter").unwrap().unwrap()), 100);
}

/// Poll `done` until it holds, backing off up to 50 ms, or fail at the deadline.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backoff = Duration::from_millis(1);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

#[test]
fn the_background_worker_folds_above_a_live_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        counter_options(true)
            .max_background_compactions(1)
            .l0_compaction_trigger(2),
    )
    .unwrap();
    db.put(b"counter", &100i64.to_be_bytes()).unwrap();
    let snapshot = db.snapshot();
    for _ in 0..2 {
        for _ in 0..OPERANDS / 2 {
            db.merge(b"counter", &1i64.to_be_bytes()).unwrap();
        }
        db.flush().unwrap();
    }

    // Two files reach the trigger, and the worker compacts them to L1.
    wait_until("the worker to empty level 0", || {
        db.get_int_property("regolith.num-files-at-level0") == Some(0)
    });

    // A thousand unfolded operands take more than 8 KiB.
    assert!(sst_bytes(&db) < 4 * 1024, "{} bytes", sst_bytes(&db));
    assert_eq!(counter(&db), 100 + OPERANDS);
    assert_eq!(int(snapshot.get(b"counter").unwrap().unwrap()), 100);
}

// --- compaction filters -------------------------------------------------

/// Whether any SSTable under `dir` holds `needle`.
fn tables_hold(env: &MemEnv, dir: &Path, needle: &[u8]) -> bool {
    env.read_dir(dir).unwrap().any(|entry| {
        let entry = entry.unwrap();
        if entry.is_dir {
            tables_hold(env, &entry.path, needle)
        } else if entry.path.extension().is_some_and(|ext| ext == "sst") {
            env.read(&entry.path)
                .unwrap()
                .windows(needle.len())
                .any(|window| window == needle)
        } else {
            false
        }
    })
}

#[test]
fn an_expired_value_is_reclaimed_by_compaction_while_a_snapshot_is_live() {
    const TTL: u64 = 60;
    const MARKER: &[u8] = b"expired-value-marker-0123456789";
    let root = Path::new("/ttl");
    let env = MemEnv::new();
    env.set_clocks(Some(0), Some(1_000_000));
    let db = DbWithTtl::open(
        root,
        Options::default()
            .env(Arc::new(env.clone()))
            .compression(CompressionType::None)
            .max_background_compactions(0),
        TTL,
    )
    .unwrap();

    db.put(b"stale", MARKER).unwrap();
    let snapshot = db.inner().snapshot();
    db.inner().flush().unwrap();
    assert!(tables_hold(&env, root, MARKER), "the value is in a table");

    // The value outlives its TTL, and something newer lands above the snapshot.
    env.advance_micros((TTL + 1) * 1_000_000);
    db.put(b"fresh", b"live").unwrap();
    db.inner().flush().unwrap();
    assert_eq!(
        db.get(b"stale").unwrap(),
        None,
        "expired values read absent"
    );

    db.compact_range(None, None).wait().unwrap();

    assert!(
        !tables_hold(&env, root, MARKER),
        "compaction reclaimed the expired value although a snapshot is live"
    );
    assert!(tables_hold(&env, root, b"live"), "the live value stays");
    assert_eq!(db.get(b"stale").unwrap(), None);
    assert_eq!(db.get(b"fresh").unwrap(), Some(b"live".to_vec()));
    // The filter ran on the stripe the snapshot reads, so the snapshot no
    // longer finds the value either: through the wrapper it was absent already.
    assert_eq!(snapshot.get(b"stale").unwrap(), None);
}

/// Keeps every value and removes every range tombstone.
struct DropRangeDeletes;

impl CompactionFilter for DropRangeDeletes {
    fn name(&self) -> &'static str {
        "drop-range-deletes"
    }

    fn filter(&self, _level: usize, _key: &[u8], _value: &[u8]) -> CompactionDecision {
        CompactionDecision::Keep
    }

    fn filter_range_delete(&self, _level: usize, _start: &[u8], _end: &[u8]) -> CompactionDecision {
        CompactionDecision::Remove
    }
}

#[test]
fn the_range_delete_filter_runs_while_a_snapshot_is_live() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .compaction_filter(Some(Arc::new(DropRangeDeletes)))
            .max_background_compactions(0),
    )
    .unwrap();
    for key in b'a'..=b'f' {
        db.put(&[key], &[key]).unwrap();
    }
    db.delete_range(b"b", b"e").unwrap();
    let snapshot = db.snapshot();
    assert_eq!(snapshot.get(b"c").unwrap(), None, "the range delete holds");
    db.flush().unwrap();

    db.compact_range(None, None).wait().unwrap();

    // The filter removed the tombstone, so the values it hid read again, to
    // the snapshot as well as to the head.
    for key in b'a'..=b'f' {
        assert_eq!(db.get(&[key]).unwrap(), Some(vec![key]));
        assert_eq!(snapshot.get(&[key]).unwrap(), Some(vec![key]));
    }
}

// --- DefraLevel ---------------------------------------------------------

/// Whether a commit failed on a conflict.
fn conflicted<T>(result: TxResult<T>) -> bool {
    matches!(result, Err(TransactionError::Conflict { .. }))
}

#[test]
fn a_blind_merge_commits_after_the_operands_beside_it_were_compacted() {
    for fold in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), counter_options(fold)).unwrap();
        db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

        let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
        merge_ones(db.db());
        db.db().flush().unwrap();
        let flushed = sst_bytes(db.db());
        db.db().compact_range(None, None).wait().unwrap();
        if fold {
            assert!(
                sst_bytes(db.db()) < flushed / 4,
                "the operands folded although the transaction holds a snapshot"
            );
        }

        tx.commit()
            .expect("operands above its snapshot never conflict");
        assert_eq!(counter(db.db()), OPERANDS + 1, "fold={fold}");
    }
}

/// A write that replaces a key outright instead of building on it.
#[derive(Clone, Copy, Debug)]
enum Replacement {
    Put,
    Delete,
    RangeDelete,
}

impl Replacement {
    fn apply(self, db: &Db) {
        match self {
            Self::Put => db.put(b"counter", &5i64.to_be_bytes()).unwrap(),
            Self::Delete => db.delete(b"counter").unwrap(),
            Self::RangeDelete => db.delete_range(b"c", b"d").unwrap(),
        }
    }

    /// The counter once a thousand operands have landed on the replacement.
    fn counter_after_operands(self) -> i64 {
        match self {
            Self::Put => 5 + OPERANDS,
            Self::Delete | Self::RangeDelete => OPERANDS,
        }
    }
}

#[test]
fn a_replacement_above_the_snapshot_still_conflicts_after_the_operands_over_it_were_compacted() {
    for replacement in [
        Replacement::Put,
        Replacement::Delete,
        Replacement::RangeDelete,
    ] {
        for fold in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let db = OptimisticTransactionDb::open(dir.path(), counter_options(fold)).unwrap();
            db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

            let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
            tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
            replacement.apply(db.db());
            merge_ones(db.db());
            db.db().flush().unwrap();
            let flushed = sst_bytes(db.db());
            db.db().compact_range(None, None).wait().unwrap();

            // The operands fold onto a put or delete above the snapshot
            // whatever the operator, and into one another only if it can.
            let folds = fold || !matches!(replacement, Replacement::RangeDelete);
            assert_eq!(
                sst_bytes(db.db()) < flushed / 4,
                folds,
                "{replacement:?} fold={fold}: {} bytes from {flushed}",
                sst_bytes(db.db())
            );
            assert!(
                conflicted(tx.commit()),
                "{replacement:?} fold={fold}: the replacement survived compaction"
            );
            assert_eq!(
                counter(db.db()),
                replacement.counter_after_operands(),
                "{replacement:?} fold={fold}"
            );
        }
    }
}
