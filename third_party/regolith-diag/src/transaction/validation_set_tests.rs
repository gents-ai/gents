//! Equivalence check: the `ValidationSet` this branch builds names exactly
//! the same (key, observed sequence, read-or-not) triples the old
//! `BTreeMap`-based `validation_set` did, and what a commit hands the engine
//! on top of it is what the class of each key says.
//!
//! Half the cases install a fake [`KeyClassifier`] that names a key of every
//! class. Only an optimistic transaction at `DefraLevel` consults it, so the
//! other cases pin that it changes nothing for anyone else.

use super::*;
use crate::column_family::CF_PREFIX_LEN;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use tempfile::TempDir;

/// Appends every operand to the base. These tests need a database that
/// accepts merges, not any particular fold.
struct Append;

impl crate::MergeOperator for Append {
    fn name(&self) -> &'static str {
        "append"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands
            .iter()
            .for_each(|operand| out.extend_from_slice(operand));
        Some(out)
    }
}

fn with_operator() -> Options {
    Options::default().merge_operator(Some(Arc::new(Append)))
}

/// The key a generated index stands for: two bytes, the first naming its
/// class to [`Classes`] and the second keeping the keys in index order.
/// Indexes 0 and 1 are ordinary, 2 and 3 share a commutative prefix, 4 is
/// content-addressed, 5 is a log key, and those past the seeded six are
/// ordinary.
fn name(index: u8) -> Vec<u8> {
    let class = match index {
        0 | 1 => b'a',
        2 | 3 => b'p',
        4 => b'q',
        5 => b'r',
        _ => b'z',
    };
    vec![class, b'0' + index]
}

/// The fake classifier: a key's first byte is its class.
struct Classes;

impl KeyClassifier for Classes {
    fn classify(&self, key: &[u8]) -> KeyClass {
        match key.first() {
            Some(b'p') => KeyClass::CommutativePrefix { len: 1 },
            Some(b'q') => KeyClass::ContentAddressed,
            Some(b'r') => KeyClass::Log,
            _ => KeyClass::Ordinary,
        }
    }
}

/// The class of the key index `index` stands for.
fn class(index: u8) -> KeyClass {
    Classes.classify(&name(index))
}

/// The index a prefixed key made by [`name`] stands for.
fn index_of(prefixed: &[u8]) -> u8 {
    prefixed[CF_PREFIX_LEN + 1] - b'0'
}

/// Whether a transaction of this flavor at this level consults the
/// classifier: only an optimistic one at `DefraLevel` on a database that
/// installed one.
fn consults(flavor: u8, level: IsolationLevel, classified: bool) -> bool {
    flavor == 0 && classified && level == IsolationLevel::DefraLevel
}

/// The dedupe `commit_inner` ran before this branch (a `HashSet` over
/// cloned keys) followed by the old `validation_set` body (a `BTreeMap`
/// keyed by the flattened `(seq, read)` pair). Kept verbatim as the
/// oracle the proptest below checks the new code against; nothing here
/// is meant to be idiomatic, only faithful to what shipped before.
fn oracle(
    tx: &Transaction,
    tracked: Vec<(Vec<u8>, Arc<KeyState>)>,
    writes: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    merges: &[(Vec<u8>, Vec<u8>)],
) -> BTreeMap<Vec<u8>, (u64, bool)> {
    let mut seen = std::collections::HashSet::new();
    let tracked: Vec<(Vec<u8>, Arc<KeyState>)> = tracked
        .into_iter()
        .filter(|(key, _)| seen.insert(key.clone()))
        .collect();

    let optimistic = matches!(tx.mode, TxMode::Optimistic);
    let serializable = matches!(
        tx.isolation,
        IsolationLevel::RepeatableRead | IsolationLevel::Serializable | IsolationLevel::DefraLevel
    );
    let read_committed = tx.isolation == IsolationLevel::ReadCommitted;
    let mut checks: BTreeMap<Vec<u8>, (u64, bool)> = BTreeMap::new();
    for (key, state) in tracked {
        let written = writes.contains_key(&key) || merges.iter().any(|(merged, _)| *merged == key);
        let validate = if serializable {
            true
        } else if read_committed {
            written
        } else {
            state.for_update.load(Ordering::Acquire) || written
        };
        if validate {
            checks.insert(key, (state.first_read_seq, true));
        }
    }
    if optimistic {
        for key in writes.keys() {
            checks
                .entry(key.clone())
                .or_insert((tx.snapshot_seq, false));
        }
        for (key, _) in merges {
            checks
                .entry(key.clone())
                .or_insert((tx.snapshot_seq, false));
        }
    }
    checks
}

/// Flatten a `ValidationSet` back into the old shape, so it can be
/// compared against [`oracle`] directly.
fn expand(
    checks: &ValidationSet,
    writes: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    merges: &[(Vec<u8>, Vec<u8>)],
) -> BTreeMap<Vec<u8>, (u64, bool)> {
    let mut out: BTreeMap<Vec<u8>, (u64, bool)> = checks
        .reads
        .iter()
        .map(|read| (read.key.clone(), (read.observed_seq, true)))
        .collect();
    if let Some(observed_seq) = checks.writes_at {
        for key in writes.keys() {
            out.entry(key.clone()).or_insert((observed_seq, false));
        }
        for (key, _) in merges {
            out.entry(key.clone()).or_insert((observed_seq, false));
        }
    }
    out
}

/// Either transaction flavor behind one type, so the op loop below does
/// not have to be written out twice.
enum AnyDb {
    Optimistic(OptimisticTransactionDb),
    Pessimistic(TransactionDb),
}

impl AnyDb {
    fn begin(&self, isolation: IsolationLevel) -> Transaction {
        match self {
            AnyDb::Optimistic(db) => db.begin(&TxnOptions::new().isolation(isolation)),
            AnyDb::Pessimistic(db) => db.begin(&TxnOptions::new().isolation(isolation)),
        }
    }

    /// A write around the transaction API, exactly like a concurrent
    /// writer would make one. Used to advance the horizon so a
    /// pessimistic `get_for_update` anchors above the begin snapshot.
    fn put_external(&self, key: &[u8], value: &[u8]) -> Result<()> {
        match self {
            AnyDb::Optimistic(db) => db.db().put(key, value),
            AnyDb::Pessimistic(db) => db.db().put(key, value),
        }
    }

    /// A delete around the transaction API.
    fn delete_external(&self, key: &[u8]) -> Result<()> {
        match self {
            AnyDb::Optimistic(db) => db.db().delete(key),
            AnyDb::Pessimistic(db) => db.db().delete(key),
        }
    }
}

fn isolation_level(level: u8) -> IsolationLevel {
    match level % 5 {
        0 => IsolationLevel::ReadCommitted,
        1 => IsolationLevel::SnapshotIsolation,
        2 => IsolationLevel::RepeatableRead,
        3 => IsolationLevel::Serializable,
        _ => IsolationLevel::DefraLevel,
    }
}

/// A database of `flavor`, with the fake classifier installed when
/// `classified`. Only the optimistic flavor takes one.
fn open_any(flavor: u8, classified: bool, dir: &TempDir) -> AnyDb {
    if flavor == 0 {
        let db = OptimisticTransactionDb::open(dir.path(), with_operator()).expect("open");
        AnyDb::Optimistic(if classified {
            db.with_policy(Arc::new(Classes))
        } else {
            db
        })
    } else {
        AnyDb::Pessimistic(
            TransactionDb::open(dir.path(), with_operator())
                .expect("open")
                .with_lock_timeout(Duration::from_secs(10)),
        )
    }
}

/// One of the three buffered writes, by the number its generator gives it.
fn buffer(tx: &Transaction, kind: u8, key: &[u8]) -> TxResult<()> {
    match kind {
        2 => tx.put(key, b"v"),
        3 => tx.delete(key),
        _ => tx.merge(key, b"op"),
    }
}

/// Whether `result` is the refusal of a write to a log key.
fn refused_as_a_log_key(result: &TxResult<()>) -> bool {
    matches!(result, Err(TransactionError::Engine(Error::LogKeyWrite)))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// For every mix of reads, writes and merges a transaction can buffer,
    /// the new `ValidationSet` names the same keys, at the same anchor,
    /// with the same read-or-not distinction, as the old flattened map did.
    /// A classifier leaves that set alone: the classes are applied on top of
    /// it. It is consulted before that only to refuse a write to a log key,
    /// which then buffers nothing.
    #[test]
    fn new_validation_set_equals_the_old_one(
        flavor in 0..2u8,
        level in 0..5u8,
        classified in any::<bool>(),
        ops in proptest::collection::vec((0u8..6, 0u8..6), 0..24),
    ) {
        let dir = TempDir::new().expect("tempdir");
        let db = open_any(flavor, classified, &dir);
        let level = isolation_level(level);
        let consults = consults(flavor, level, classified);

        let mut tx = db.begin(level);
        for (kind, index) in &ops {
            let key = name(*index);
            match kind {
                0 => prop_assert!(tx.get(&key).is_ok()),
                1 => prop_assert!(tx.get_for_update(&key).is_ok()),
                2..=4 => {
                    let result = buffer(&tx, *kind, &key);
                    if consults && class(*index) == KeyClass::Log {
                        prop_assert!(refused_as_a_log_key(&result), "{:?}", result);
                    } else {
                        prop_assert!(result.is_ok(), "{:?}", result);
                    }
                }
                _ => prop_assert!(db.put_external(&key, b"v").is_ok()),
            }
        }

        // Drained exactly as `commit_inner` drains them. This never
        // reaches the engine: nothing here commits.
        let (writes, merges) = settle(tx.writes.drain());
        let tracked = tx.tracked.drain();
        if consults {
            prop_assert!(
                writes.keys().chain(merges.iter().map(|(key, _)| key))
                    .all(|key| class(index_of(key)) != KeyClass::Log),
                "a refused write of a log key was buffered"
            );
        }

        let want = oracle(&tx, tracked.clone(), &writes, &merges);
        let checks = tx.validation_set(tracked, &writes, &merges);
        prop_assert!(
            checks.reads.windows(2).all(|pair| pair[0].key < pair[1].key),
            "reads must be strictly ascending by key with no duplicates"
        );
        prop_assert_eq!(
            checks.blind_merges_commute,
            tx.isolation == IsolationLevel::DefraLevel,
            "only DefraLevel lets blind merges commute"
        );
        let got = expand(&checks, &writes, &merges);
        prop_assert_eq!(got, want);

        drop(tx);
    }
}

/// Duplicate cells for one key reach `validation_set` only when two
/// threads race `get_or_insert` on a shared transaction (`observe` in the
/// parent module): the proptest above drives one op sequence per
/// transaction, so `tracked` never holds two cells for the same key there
/// and the `sort_by` + `dedup_by` in `validation_set` goes unexercised.
/// These two cases build `tracked` directly with duplicates instead.
///
/// Neither case alone is enough. Case 1 mixes `for_update` values, so a
/// missing dedupe can still pass by coincidence: the `SnapshotIsolation`
/// filter drops the losing (`for_update = false`) cell on its own. Case 2
/// gives both cells the same `for_update`, so only the dedupe itself can
/// collapse them to one.
#[test]
fn duplicate_tracked_cells_are_deduped_keeping_the_newest() {
    let dir = TempDir::new().expect("tempdir");
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).expect("open");
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    let seq = tx.snapshot_seq;

    // Case 1: catches a dedupe that keeps the wrong (older) cell. Newest
    // first, as the drain yields them; the newest cell carries
    // `for_update`, and keeping the other one instead would drop this
    // `get_for_update` key from validation, a lost update.
    let tracked = vec![
        (b"k".to_vec(), Arc::new(KeyState::new(seq, true))),
        (b"k".to_vec(), Arc::new(KeyState::new(seq, false))),
    ];
    let checks = tx.validation_set(tracked, &BTreeMap::new(), &[]);
    assert_eq!(checks.reads.len(), 1, "a duplicate key validates once");
    assert_eq!(checks.reads[0].key.as_slice(), b"k");

    // Case 2: catches a missing dedupe outright. Both cells carry
    // `for_update`, so a missing dedupe validates the key twice.
    let tracked = vec![
        (b"k".to_vec(), Arc::new(KeyState::new(seq, true))),
        (b"k".to_vec(), Arc::new(KeyState::new(seq, true))),
    ];
    let checks = tx.validation_set(tracked, &BTreeMap::new(), &[]);
    assert_eq!(
        checks.reads.len(),
        1,
        "a key is validated once however many cells it has"
    );

    drop(tx);
}

/// F2: a transaction that writes nothing, at a level that checks none of
/// its reads, hands the engine a set with no read and leaves its reads
/// undrained. A `get_for_update` at `SnapshotIsolation` or under DefraLevel's
/// write-free relaxation, and any read at a level that checks every read,
/// builds the set from the drained reads, and the read is in it. Which path
/// a commit takes changes no set: `a_scan_validates_like_gets_of_what_it_walked`
/// compares both against the twin's built set, and
/// `a_validated_scan_that_writes_nothing_is_checked` shows a validated
/// range kept on the short path.
#[test]
fn a_write_free_commit_builds_no_set_where_its_level_checks_nothing() {
    for flavor in 0..2u8 {
        for level in (0..5u8).map(isolation_level) {
            for for_update in [false, true] {
                let dir = TempDir::new().expect("tempdir");
                let db = open_any(flavor, false, &dir);
                db.put_external(b"k", b"v").expect("put");
                let mut tx = db.begin(level);
                if for_update {
                    tx.get_for_update(b"k").expect("read");
                } else {
                    tx.get(b"k").expect("read");
                }
                let relaxed = flavor == 0 && level == IsolationLevel::DefraLevel;
                let nothing = match level {
                    IsolationLevel::ReadCommitted => true,
                    IsolationLevel::SnapshotIsolation => !for_update,
                    IsolationLevel::DefraLevel if relaxed => !for_update,
                    _ => false,
                };
                let case = format!("flavor {flavor}, {level:?}, for update {for_update}");
                let drained = tx.drain_for_commit().expect("drain");
                assert!(drained.write_free, "{case}");
                assert_eq!(
                    drained.commit.checks.reads.is_empty(),
                    nothing,
                    "{case}: the set"
                );
                assert_eq!(
                    tx.tracked.len(),
                    usize::from(nothing),
                    "{case}: the reads drained"
                );
                drop(drained);
            }
        }
    }
}

/// Whether the transaction buffered a merge operand for `prefixed`.
fn has_operands(tx: &Transaction, prefixed: &[u8]) -> bool {
    let mut found = false;
    tx.writes.walk_chain(prefixed, |write| {
        found |= matches!(write, Write::Merge(_));
        ControlFlow::Continue(())
    });
    found
}

/// Whether merge operands were buffered for `prefixed` after its newest put
/// or delete, so the key reads as those operands applied to that entry.
fn operands_above_terminator(tx: &Transaction, prefixed: &[u8]) -> bool {
    let mut found = false;
    tx.writes.walk_chain(prefixed, |write| {
        if write.is_terminator() {
            return ControlFlow::Break(());
        }
        found |= matches!(write, Write::Merge(_));
        ControlFlow::Continue(())
    });
    found
}

/// The put or delete the transaction buffered for `prefixed`, if any. Merge
/// operands alone are not one: a scan reads the entry beneath them, and
/// yields the key with the operands applied even when the snapshot holds
/// nothing there.
fn replacement(tx: &Transaction, prefixed: &[u8]) -> Option<Write> {
    let mut found = None;
    tx.writes.walk_chain(prefixed, |write| {
        found = write.is_terminator().then(|| write.clone());
        if found.is_some() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    found
}

/// Whether the stretch from key index `a` to key index `b` stays inside one
/// commutative prefix: both keys are of that class and share the byte the
/// class takes for its prefix.
fn inside_one_commutative_prefix(a: u8, b: u8) -> bool {
    matches!(class(a), KeyClass::CommutativePrefix { .. }) && name(a)[0] == name(b)[0]
}

/// The keys the stretches of one scan of `[start, end)` cover, worked out
/// independently of `TxnScanStream`: the snapshot holds exactly the keys in
/// `seeded`, an entry of the write buffer ends a stretch (and is yielded when
/// it is a put, or a delete with merge operands above it), a snapshot key already promoted past the begin snapshot ends
/// a stretch (and is yielded when the engine has it at its read sequence),
/// every other snapshot key extends the stretch, a key that only has merge
/// operands is read like a snapshot key whether or not the snapshot holds it
/// and is always yielded, and the walk stops once `take` entries were
/// yielded. With `classified`, a stretch that stays inside one commutative
/// prefix covers nothing: the commit drops it. Called before the scan runs.
fn expected_cover(
    tx: &Transaction,
    seeded: u8,
    start: u8,
    end: u8,
    reverse: bool,
    take: usize,
    classified: bool,
) -> BTreeSet<Vec<u8>> {
    let mut covered = BTreeSet::new();
    let mut close = |stretch: &mut Option<(u8, u8)>| {
        if let Some((a, b)) = stretch.take()
            && !(classified && inside_one_commutative_prefix(a, b))
        {
            for key in a.min(b)..=a.max(b) {
                covered.insert(prefix_key(DEFAULT_CF_ID, &name(key)));
            }
        }
    };
    let keys: Vec<u8> = if reverse {
        (start..end).rev().collect()
    } else {
        (start..end).collect()
    };
    let mut stretch: Option<(u8, u8)> = None;
    let mut yielded = 0;
    for key in keys {
        if yielded == take {
            break;
        }
        let prefixed = prefix_key(DEFAULT_CF_ID, &name(key));
        if let Some(replaced) = replacement(tx, &prefixed) {
            close(&mut stretch);
            // A put reads as a value, and so does a delete with operands
            // buffered above it: they apply to nothing and yield.
            yielded += usize::from(
                matches!(replaced, Write::Put(_)) || operands_above_terminator(tx, &prefixed),
            );
            continue;
        }
        let merged = has_operands(tx, &prefixed);
        if !merged && (key >= 6 || seeded & (1 << key) == 0) {
            continue;
        }
        let promoted = matches!(tx.mode, TxMode::Pessimistic)
            .then(|| tx.tracked.get(&prefixed))
            .flatten()
            .map(|state| state.read_seq.load(Ordering::Acquire))
            .filter(|read_seq| *read_seq > tx.snapshot_seq);
        if let Some(read_seq) = promoted {
            close(&mut stretch);
            let visible = tx
                .engine
                .get_slice_at(&prefixed, read_seq)
                .expect("engine read");
            // Operands apply to nothing when the key is not visible.
            yielded += usize::from(merged || visible.is_some());
            continue;
        }
        stretch = Some(stretch.map_or((key, key), |(first, _)| (first, key)));
        yielded += 1;
    }
    close(&mut stretch);
    covered
}

/// How a commit validates one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    /// As a write, against the begin snapshot.
    Write,
    /// As a read, in whole.
    Read,
    /// As a read that only needs the key to be there still.
    Presence,
}

/// What a drained commit hands the engine, flattened: each key it validates
/// with the sequence and the kind of check. A written key the classifier
/// exempted is in neither loop of the engine, so it is not here.
fn handed_over(commit: &crate::engine::commit::TxnCommit) -> BTreeMap<Vec<u8>, (u64, Check)> {
    let checks = &commit.checks;
    let mut out: BTreeMap<Vec<u8>, (u64, Check)> = checks
        .reads
        .iter()
        .map(|read| {
            let check = if read.presence_only() {
                Check::Presence
            } else {
                Check::Read
            };
            (read.key.clone(), (read.observed_seq, check))
        })
        .collect();
    if let Some(observed_seq) = checks.writes_at {
        let written = commit
            .point_ops
            .keys()
            .chain(commit.merges.iter().map(|(key, _)| key));
        for key in written.filter(|key| !checks.exempt.contains(*key)) {
            out.entry(key.clone())
                .or_insert((observed_seq, Check::Write));
        }
    }
    out
}

/// A transaction that scans validates exactly what a twin that reads
/// the same snapshot keys through `get` validates, except that every
/// written or validated key inside a stretch the scans walked is a read
/// from the begin snapshot. The stretches are worked out by
/// [`expected_cover`], not by the code under test.
///
/// At `RepeatableRead` the twin does not read what the scan yielded: a `get`
/// there is validated and a scanned key is not, so the scan must leave
/// exactly what a transaction that never read those keys leaves, plus
/// the anchor.
///
/// The scanning transaction is drained as its commit drains it, so what
/// is compared is what the engine is handed. Where it consults the fake
/// classifier, the classes are applied to the expectation here, by rules
/// written out again and not by the code under test:
///
/// * a stretch inside one commutative prefix covers nothing;
/// * a read of a log key is not validated, and a write of one is refused
///   and buffers nothing;
/// * a content-addressed key the commit puts or merges is exempt, write
///   and read alike, and listed as exempt; one it only read, or deletes,
///   is validated for presence when a point read found it in the database
///   and in whole otherwise, a read its own delete answered included.
///
/// An optimistic transaction at `DefraLevel` that buffered nothing is
/// write-free: it keeps only the reads it made through `get_for_update`,
/// and its scans cover nothing.
fn scan_against_twin(
    flavor: u8,
    level: u8,
    classified: bool,
    seeded: u8,
    ops: &[(u8, u8)],
) -> std::result::Result<(), TestCaseError> {
    let (scan_dir, twin_dir) = (
        TempDir::new().expect("tempdir"),
        TempDir::new().expect("tempdir"),
    );
    // The twin never has a classifier: it is the unclassified base.
    let (scan_db, twin_db) = (
        open_any(flavor, classified, &scan_dir),
        open_any(flavor, false, &twin_dir),
    );
    let level = isolation_level(level);
    let consults = consults(flavor, level, classified);
    // Committed before the transactions begin, so the snapshot holds
    // exactly these keys; a write after begin is invisible to a scan.
    for key in (0u8..6).filter(|key| seeded & (1 << key) != 0) {
        prop_assert!(scan_db.put_external(&name(key), b"s").is_ok());
        prop_assert!(twin_db.put_external(&name(key), b"s").is_ok());
    }
    let mut scan_tx = scan_db.begin(level);
    let mut twin_tx = twin_db.begin(level);
    let mut covered = BTreeSet::new();
    // The keys a point read asked the database for: one made while the
    // transaction had no put or delete of the key buffered. A read made
    // after one is answered by the buffer, and found nothing there.
    let mut asked_the_database = BTreeSet::new();
    for (kind, index) in ops {
        let key = name(*index);
        let prefixed = prefix_key(DEFAULT_CF_ID, &key);
        if *kind < 2 && replacement(&scan_tx, &prefixed).is_none() {
            asked_the_database.insert(prefixed);
        }
        if consults && class(*index) == KeyClass::Log && (2..=4).contains(kind) {
            // Refused, so neither transaction makes the write.
            let result = buffer(&scan_tx, *kind, &key);
            prop_assert!(refused_as_a_log_key(&result), "{:?}", result);
            continue;
        }
        for (db, tx) in [(&scan_db, &scan_tx), (&twin_db, &twin_tx)] {
            match kind {
                0 => prop_assert!(tx.get(&key).is_ok()),
                1 => prop_assert!(tx.get_for_update(&key).is_ok()),
                2..=4 => prop_assert!(buffer(tx, *kind, &key).is_ok()),
                5 => prop_assert!(db.put_external(&key, b"v").is_ok()),
                6 => prop_assert!(db.delete_external(&key).is_ok()),
                _ => {}
            }
        }
        if *kind >= 7 {
            let end = name(*index + 4);
            let reverse = *kind == 8;
            let take = if *kind == 9 { 1 } else { usize::MAX };
            covered.extend(expected_cover(
                &scan_tx,
                seeded,
                *index,
                *index + 4,
                reverse,
                take,
                consults,
            ));
            let direction = if reverse {
                ScanDirection::Reverse
            } else {
                ScanDirection::Forward
            };
            let mut stream = scan_tx.scan_stream_in(Some(&key), Some(&end), direction);
            let entries: Vec<Vec<u8>> = stream
                .by_ref()
                .take(take)
                .map(|item| item.unwrap().0)
                .collect();
            drop(stream);
            for entry in entries {
                let buffered = replacement(&scan_tx, &prefix_key(DEFAULT_CF_ID, &entry)).is_some();
                if !buffered
                    && !matches!(
                        scan_tx.isolation,
                        IsolationLevel::RepeatableRead | IsolationLevel::DefraLevel
                    )
                {
                    prop_assert!(twin_tx.get(&entry).is_ok());
                }
            }
        }
    }

    // The twin: its point reads and writes, before any class or scan.
    let (twin_writes, twin_merges) = write_buffer::settle(twin_tx.writes.drain());
    let twin_tracked = twin_tx.tracked.drain();
    let twin_checks = twin_tx.validation_set(twin_tracked, &twin_writes, &twin_merges);
    let twin_set = expand(&twin_checks, &twin_writes, &twin_merges);
    let asked: BTreeSet<&[u8]> = twin_checks
        .reads
        .iter()
        .filter(|read| read.access == Access::ReadForUpdate)
        .map(|read| read.key.as_slice())
        .collect();
    let point_read = |key: &[u8]| twin_set.get(key).is_some_and(|(_, read)| *read);

    // The scanning transaction: what its commit hands the engine.
    let begin_seq = scan_tx.snapshot_seq;
    let drained = scan_tx.drain_for_commit().expect("drain");
    let got = handed_over(&drained.commit);
    let (writes, merges) = (&drained.commit.point_ops, &drained.commit.merges);
    let written: BTreeSet<Vec<u8>> = writes
        .keys()
        .chain(merges.iter().map(|(key, _)| key))
        .cloned()
        .collect();
    prop_assert_eq!(
        &twin_writes,
        writes,
        "the twin buffered what the scanner did"
    );

    // Writing nothing takes the write-free commit at every level; only an
    // optimistic transaction at DefraLevel relaxes what it checks.
    let write_free = written.is_empty();
    prop_assert_eq!(drained.write_free, write_free);
    let relaxed = write_free && flavor == 0 && level == IsolationLevel::DefraLevel;
    let mut want: BTreeMap<Vec<u8>, (u64, Check)> = twin_set
        .iter()
        .filter(|(key, _)| !relaxed || asked.contains(key.as_slice()))
        .map(|(key, (seq, read))| {
            (
                key.clone(),
                (*seq, if *read { Check::Read } else { Check::Write }),
            )
        })
        .collect();
    if !relaxed {
        for key in &covered {
            if written.contains(key) || point_read(key) {
                want.insert(key.clone(), (begin_seq, Check::Read));
            }
        }
    }
    let mut exempt: Vec<Vec<u8>> = Vec::new();
    if consults {
        let put = |key: &Vec<u8>| matches!(writes.get(key), Some(Some(_)));
        let merged = |key: &Vec<u8>| merges.iter().any(|(into, _)| into == key);
        exempt = written
            .iter()
            .filter(|key| {
                let key: &Vec<u8> = key;
                class(index_of(key)) == KeyClass::ContentAddressed && (put(key) || merged(key))
            })
            .cloned()
            .collect();
        want.retain(|key, (_, check)| {
            if exempt.contains(key) {
                return false;
            }
            let index = index_of(key);
            // A point read found the key when it asked the database
            // and the snapshot held the key.
            let found = asked_the_database.contains(key) && seeded & (1 << index) != 0;
            match (class(index), *check) {
                (KeyClass::Log, Check::Read) => false,
                (KeyClass::ContentAddressed, Check::Read) if found => {
                    *check = Check::Presence;
                    true
                }
                _ => true,
            }
        });
    }
    prop_assert_eq!(&drained.commit.checks.exempt, &exempt);
    prop_assert_eq!(got, want);
    drop(drained);
    drop(scan_tx);
    drop(twin_tx);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// [`scan_against_twin`] for both flavors at every level, with and
    /// without the fake classifier: where it is not consulted, it changes
    /// nothing.
    #[test]
    fn a_scan_validates_like_gets_of_what_it_walked(
        flavor in 0..2u8,
        level in 0..5u8,
        classified in any::<bool>(),
        seeded in any::<u8>(),
        ops in proptest::collection::vec((0u8..10, 0u8..6), 0..28),
    ) {
        scan_against_twin(flavor, level, classified, seeded, &ops)?;
    }

    /// [`scan_against_twin`] where every case consults the fake classifier:
    /// an optimistic transaction at `DefraLevel` over keys of every class.
    #[test]
    fn a_classified_commit_at_defra_level_validates_what_each_class_says(
        seeded in any::<u8>(),
        ops in proptest::collection::vec((0u8..10, 0u8..6), 0..28),
    ) {
        scan_against_twin(0, 4, true, seeded, &ops)?;
    }
}
