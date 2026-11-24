//! What a transactional scan records, and what the commit validates
//! because of it.
//!
//! At Serializable a scan records every snapshot key it yields in the same
//! read set a point `get` uses. At every level it records each unbroken
//! stretch of snapshot keys it yielded, and a key inside a stretch that the
//! transaction then writes is validated as a read from the begin snapshot.
//! A bound key the walk stopped on is outside every stretch, an entry from
//! the transaction's own writes ends a stretch, and a key already locked
//! through `get_for_update` is served at its lock horizon. A phantom - a key
//! inserted into the range after the snapshot - is not detected unless the
//! transaction validates that key anyway.

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use proptest::prelude::*;
use regolith::{
    IsolationLevel, OptimisticTransactionDb, Options, ScanDirection, TransactionDb,
    TransactionError, TxnOptions,
};
use tempfile::TempDir;

mod common;

fn opt_db(dir: &TempDir) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap()
}

fn pes_db(dir: &TempDir) -> TransactionDb {
    TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(Duration::from_secs(10))
}

/// Write `keys` at `b"0"`. Callers pass exactly the keys a test's
/// bounds and assertions depend on, since a key outside that set could
/// land inside a scanned range and change what a test's exact key list
/// asserts.
fn seed(db: &regolith::Db, keys: &[&[u8]]) {
    for key in keys {
        db.put(key, b"0").unwrap();
    }
}

/// Collect a scan's keys, so every test asserts the scan actually
/// yielded the key it reasons about before reasoning about the commit.
/// A scan that silently yielded nothing would prove nothing.
fn scanned_keys(stream: regolith::TxnScanStream<'_>) -> Vec<Vec<u8>> {
    stream.map(|item| item.unwrap().0).collect()
}

fn is_conflict<T>(r: &regolith::TxResult<T>) -> bool {
    matches!(r, Err(TransactionError::Conflict { .. }))
}

fn levels() -> [IsolationLevel; 4] {
    [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
    ]
}

/// T1: write skew through a scan. A key read only via a transactional
/// scan must still abort the transaction that scanned it, at
/// Serializable.
///
/// Fails under: M10, deleting the `observe` call in `yield_cursor` (the
/// scanned key stops being tracked at Serializable, so `a.commit()`
/// succeeds instead).
#[test]
fn serializable_aborts_when_a_scanned_key_is_overwritten() {
    let dir = TempDir::new().unwrap();
    let db = opt_db(&dir);
    seed(db.db(), &[b"a", b"k", b"z"]);

    let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    let seen = scanned_keys(a.scan_stream(Some(b"a"), Some(b"z")));
    assert!(
        seen.contains(&b"k".to_vec()),
        "the scan must yield k: {seen:?}"
    );

    db.db().put(b"k", b"1").unwrap();
    a.put(b"other", b"1").unwrap();

    match a.commit() {
        Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"k"),
        other => panic!("expected a conflict on k, got {other:?}"),
    }
}

/// T2: the range a scan walked is not recorded, only the keys it
/// yielded. A key the cursor stopped on at a bound, in either
/// direction, is free to change underneath the transaction.
///
/// Fails under: M16, observing the key under the cursor in `peek_cursor`
/// before the `past_bound` check (the key the walk stopped on gets
/// recorded, so both halves below abort instead of committing).
#[test]
fn serializable_ignores_keys_outside_the_scanned_range() {
    // Forward: `[a, k)` positions the cursor on `k` and rejects it at
    // the exclusive end bound.
    {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"a", b"k", b"z"]);

        let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        let seen = scanned_keys(a.scan_stream(Some(b"a"), Some(b"k")));
        assert!(
            !seen.contains(&b"k".to_vec()),
            "k must not be yielded: {seen:?}"
        );

        db.db().put(b"k", b"1").unwrap();
        a.put(b"other", b"1").unwrap();
        a.commit()
            .expect("k was never yielded, so it is not in the read set");
    }

    // Reverse: `[k2, z)` yields exactly `k2`; the walk then steps down
    // onto `k`, which is below the inclusive start bound and rejected.
    {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"a", b"k", b"k2", b"z"]);

        let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        let seen = scanned_keys(a.scan_stream_in(Some(b"k2"), Some(b"z"), ScanDirection::Reverse));
        assert_eq!(seen, [b"k2".to_vec()]);

        db.db().put(b"k", b"1").unwrap();
        a.put(b"other", b"1").unwrap();
        a.commit()
            .expect("k is below the scanned range, so it is not in the read set");
    }
}

/// T3: a scan that is later written is a read-modify-write, not a blind
/// write. Two transactions that both scan a counter and both write the
/// value they saw plus one must not both commit, at every isolation
/// level and in both transaction flavours.
///
/// Fails under: M6 (a scan's run is never registered with the
/// transaction) or M7 (`cover` adds no written key). Either way the
/// scanned key is not anchored at the begin snapshot below Serializable,
/// so the second transaction's write is validated as blind,
/// `write_matches_committed` elides it, the second transaction commits,
/// and the counter ends at 6 despite two increments having been
/// performed.
#[test]
fn a_scan_then_write_of_the_same_bytes_never_loses_an_update() {
    // Optimistic: both transactions read the same snapshot and race to
    // commit; the second must be refused even though its write is
    // byte-identical to the first's.
    for level in levels() {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        db.db().put(b"counter", &5u64.to_le_bytes()).unwrap();

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));

        for txn in [&first, &second] {
            let mut stream = txn.scan_stream(Some(b"counter"), Some(b"counter\0"));
            let (key, value) = stream
                .next()
                .unwrap_or_else(|| panic!("{level:?}: counter must be yielded"))
                .unwrap();
            assert_eq!(key, b"counter".to_vec());
            assert_eq!(u64::from_le_bytes(value.as_slice().try_into().unwrap()), 5);
            drop(stream);
            txn.put(b"counter", &6u64.to_le_bytes()).unwrap();
        }

        first
            .commit()
            .unwrap_or_else(|e| panic!("{level:?}: first commit: {e}"));
        assert!(
            is_conflict(&second.commit()),
            "{level:?}: a stale scan-then-write must not be elided as an idempotent blind write"
        );
        assert_eq!(
            u64::from_le_bytes(
                db.db()
                    .get(b"counter")
                    .unwrap()
                    .unwrap()
                    .try_into()
                    .unwrap()
            ),
            6
        );
    }

    // Pessimistic: A scans and sees 5 without locking; B locks the
    // counter through `get_for_update`, writes 6, and commits, freeing
    // the lock; A then locks it (free now) and writes 6. A's read
    // happened before B's write landed, so A must abort.
    for level in levels() {
        let dir = TempDir::new().unwrap();
        let db = pes_db(&dir);
        db.db().put(b"counter", &5u64.to_le_bytes()).unwrap();

        let a = db.begin(&TxnOptions::new().isolation(level));
        {
            let mut stream = a.scan_stream(Some(b"counter"), Some(b"counter\0"));
            let (key, value) = stream
                .next()
                .unwrap_or_else(|| panic!("{level:?}: counter must be yielded"))
                .unwrap();
            assert_eq!(key, b"counter".to_vec());
            assert_eq!(u64::from_le_bytes(value.as_slice().try_into().unwrap()), 5);
        }

        let b = db.begin(&TxnOptions::new().isolation(level));
        let current = u64::from_le_bytes(
            b.get_for_update(b"counter")
                .unwrap()
                .unwrap()
                .try_into()
                .unwrap(),
        );
        assert_eq!(current, 5);
        b.put(b"counter", &6u64.to_le_bytes()).unwrap();
        b.commit()
            .unwrap_or_else(|e| panic!("{level:?}: b commit: {e}"));

        a.put(b"counter", &6u64.to_le_bytes()).unwrap();
        assert!(
            is_conflict(&a.commit()),
            "{level:?}: a's scan-then-write must abort once b's write landed before a's lock"
        );
    }
}

/// T4: below `Serializable`, a scanned key that the transaction never
/// writes is a plain read, not a `get_for_update`, so it is not
/// validated. `ReadCommitted` validates nothing it did not write, and
/// `SnapshotIsolation` does not validate a plain read.
///
/// Fails under: M17, observing every yielded key with `for_update = true`
/// at every level (the `SnapshotIsolation` half then aborts;
/// `ReadCommitted` would still commit either way, which is why the
/// `SnapshotIsolation` half is here too).
#[test]
fn read_committed_validates_nothing_it_did_not_write() {
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
    ] {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"a", b"k", b"z"]);

        let a = db.begin(&TxnOptions::new().isolation(level));
        let seen = scanned_keys(a.scan_stream(Some(b"a"), Some(b"z")));
        assert!(
            seen.contains(&b"k".to_vec()),
            "{level:?}: scan must yield k: {seen:?}"
        );

        db.db().put(b"k", b"1").unwrap();
        a.put(b"other", b"1").unwrap();
        a.commit().unwrap_or_else(|e| panic!("{level:?}: {e}"));

        assert_eq!(db.db().get(b"k").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.db().get(b"other").unwrap(), Some(b"1".to_vec()));
    }
}

/// T5: both snapshot-yield arms of the merge record their key, not just
/// one. `k` reaches the caller through the `Ordering::Less` arm only
/// while a buffered key that comes after it in the scan's own order is
/// still pending; a snapshot key past that buffered key reaches the
/// caller through the `(Some(key), None)` arm once the buffered side is
/// exhausted. This scan exercises both.
///
/// Fails under: M18 (the `Ordering::Less` arm serves the cursor value
/// directly, bypassing `yield_cursor`, so nothing reached through it is
/// recorded) or M10 (no per-key record at Serializable, so nothing this
/// scan yields is tracked at all). `T1`, which buffers nothing, still
/// passes under M18 because its `k` reaches `(Some, None)` directly;
/// this test does not, because its `k`/`p:1`/`p:k` reach the caller
/// through `Less`.
#[test]
fn reverse_and_prefix_scans_record_their_keys_too() {
    // Reverse: buffering `b` makes `k` arrive through `Less` (the
    // cursor, at `k`, precedes the buffered `b` in descending order)
    // and `a` arrive through `(Some, None)` once `b` is drained.
    {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"a", b"k", b"z"]);

        let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        a.put(b"b", b"pending").unwrap();
        let seen = scanned_keys(a.scan_stream_in(Some(b"a"), Some(b"z"), ScanDirection::Reverse));
        assert_eq!(
            seen,
            [b"k".to_vec(), b"b".to_vec(), b"a".to_vec()],
            "k through Less, b buffered, a through (Some, None)"
        );

        db.db().put(b"k", b"1").unwrap();
        a.put(b"other", b"1").unwrap();
        match a.commit() {
            Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"k"),
            other => panic!("expected a conflict on k, got {other:?}"),
        }
    }

    // Prefix, positive half: buffering `p:m` makes both `p:1` and `p:k`
    // arrive through `Less`, and `p:z` arrive through `(Some, None)`
    // once `p:m` is drained. `q`, outside the prefix, is never in
    // range.
    {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"p:1", b"p:k", b"p:z", b"q"]);

        let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        a.put(b"p:m", b"pending").unwrap();
        let seen = scanned_keys(a.scan_stream(Some(b"p:"), Some(b"p;")));
        assert_eq!(
            seen,
            [
                b"p:1".to_vec(),
                b"p:k".to_vec(),
                b"p:m".to_vec(),
                b"p:z".to_vec(),
            ],
            "the prefix bound held and q was not yielded"
        );

        db.db().put(b"p:k", b"1").unwrap();
        a.put(b"other", b"1").unwrap();
        match a.commit() {
            Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"p:k"),
            other => panic!("expected a conflict on p:k, got {other:?}"),
        }
    }

    // Prefix, negative half: the same shape, but the concurrent write
    // lands on `q`, which is outside the scanned range and so is never
    // in the read set.
    {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"p:1", b"p:k", b"p:z", b"q"]);

        let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        a.put(b"p:m", b"pending").unwrap();
        let seen = scanned_keys(a.scan_stream(Some(b"p:"), Some(b"p;")));
        assert_eq!(
            seen,
            [
                b"p:1".to_vec(),
                b"p:k".to_vec(),
                b"p:m".to_vec(),
                b"p:z".to_vec(),
            ]
        );

        db.db().put(b"q", b"1").unwrap();
        a.put(b"other", b"1").unwrap();
        a.commit()
            .expect("q is outside the scanned prefix, so it is not in the read set");
    }
}

/// One actor's progress through `begin -> scan -> put -> commit`, so a
/// schedule bit can be mapped straight onto "take this actor's next
/// step" without re-deriving what that step is from the outside.
enum Step {
    NotBegun,
    Begun(regolith::Transaction),
    Scanned(regolith::Transaction, u64),
    Ready(regolith::Transaction),
    Committed,
}

/// The value the counter holds according to a scan of `[c, d)`, which
/// includes the counter itself and any neighbour keys T7 seeded beside
/// it, so the tracked key is not the only one this scan records.
fn scan_counter(txn: &regolith::Transaction, direction: ScanDirection) -> u64 {
    for item in txn.scan_stream_in(Some(b"c"), Some(b"d"), direction) {
        let (key, value) = item.unwrap();
        if key == b"counter" {
            return u64::from_le_bytes(value.as_slice().try_into().expect("8-byte counter"));
        }
    }
    panic!("scan of [c, d) did not yield the counter key");
}

/// Advance one actor by exactly one step. Returns the next step and
/// whether this step was a commit that came back `Conflict`, which
/// resets the actor to `NotBegun` for a fresh attempt from a new
/// snapshot.
fn advance(
    step: Step,
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    direction: ScanDirection,
) -> (Step, bool) {
    match step {
        Step::NotBegun => (
            Step::Begun(db.begin(&TxnOptions::new().isolation(level))),
            false,
        ),
        Step::Begun(txn) => {
            let seen = scan_counter(&txn, direction);
            (Step::Scanned(txn, seen), false)
        }
        Step::Scanned(txn, seen) => {
            txn.put(b"counter", &(seen + 1).to_le_bytes()).unwrap();
            (Step::Ready(txn), false)
        }
        Step::Ready(txn) => match txn.commit() {
            Ok(_) => (Step::Committed, false),
            Err(TransactionError::Conflict { .. }) => (Step::NotBegun, true),
            Err(e) => panic!("unexpected transaction error: {e}"),
        },
        Step::Committed => (Step::Committed, false),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// T7: whatever order two transactions take their `begin -> scan ->
    /// put -> commit` steps in, and however many times a conflict sends
    /// one of them back to `begin`, a counter both scanned and then
    /// incremented never loses an increment. This is T3's optimistic
    /// half, generalized over interleaving instead of fixed to one
    /// schedule.
    ///
    /// `advance` panics on any commit error other than `Conflict`, which
    /// is what guarantees that every restart came from a conflict.
    ///
    /// Fails under: not registering a scan's run (and, at Serializable,
    /// not calling `observe`) in `yield_cursor`. Any generated schedule
    /// where both actors finish their scan before either commits then
    /// ends with the counter at 1 instead of 2. About 5 in 8 generated
    /// schedules have that shape, so 64 cases all missing it has a
    /// probability below 1e-27.
    #[test]
    fn interleaved_scan_then_write_increments_are_never_lost(
        level in prop::sample::select(vec![
            IsolationLevel::SnapshotIsolation,
            IsolationLevel::Serializable,
        ]),
        direction in prop::sample::select(vec![ScanDirection::Forward, ScanDirection::Reverse]),
        schedule in prop::collection::vec(any::<bool>(), 6..16),
        neighbours in 0..4usize,
    ) {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
        db.db().put(b"counter", &0u64.to_le_bytes()).unwrap();
        for i in 0..neighbours {
            db.db().put(format!("c{i}").as_bytes(), b"0").unwrap();
        }

        let mut steps = [Step::NotBegun, Step::NotBegun];
        let mut restarts = [0u32; 2];
        let mut bits = schedule.iter().copied();
        // A step bound well above what any correct, non-livelocked run
        // needs (4 steps per actor per attempt, at most 4 attempts
        // each): a bug that spins forever fails loudly here instead of
        // hanging the suite.
        let mut fuel: usize = 4 * 4 * 2;

        loop {
            let both_done =
                matches!(steps[0], Step::Committed) && matches!(steps[1], Step::Committed);
            if both_done {
                break;
            }
            prop_assert!(fuel > 0, "ran out of steps without both actors committing");
            fuel -= 1;

            // Once bits run out, "remaining bits drive the other"
            // still applies: alternate, and the committed-actor check
            // below always redirects onto whichever one is not done.
            let mut idx = match bits.next() {
                Some(true) => 0,
                Some(false) => 1,
                None => fuel % 2,
            };
            if matches!(steps[idx], Step::Committed) {
                idx = 1 - idx;
            }

            let taken = std::mem::replace(&mut steps[idx], Step::Committed);
            let (next, conflicted) = advance(taken, &db, level, direction);
            if conflicted {
                restarts[idx] += 1;
                prop_assert!(
                    restarts[idx] <= 3,
                    "actor {idx} restarted more than 3 times: {:?} (livelock)",
                    restarts
                );
            }
            steps[idx] = next;
        }

        let counter = u64::from_le_bytes(
            db.db().get(b"counter").unwrap().unwrap().try_into().unwrap(),
        );
        prop_assert_eq!(counter, 2, "level={:?} direction={:?}", level, direction);
    }
}

fn counter_of(value: &[u8]) -> u64 {
    u64::from_le_bytes(value.try_into().expect("8-byte counter"))
}

/// The counter as a scan of `[count, counter2]` sees it. The neighbours on
/// both sides put the counter strictly inside the stretch the scan walks.
fn scan_counter_value(txn: &regolith::Transaction) -> Option<u64> {
    let mut seen = Vec::new();
    let mut counter = None;
    for item in txn.scan_stream(Some(b"count"), Some(b"counter3")) {
        let (key, value) = item.unwrap();
        if key == b"counter" {
            counter = Some(counter_of(value.as_slice()));
        }
        seen.push(key);
    }
    assert_eq!(seen.first().map(Vec::as_slice), Some(&b"count"[..]));
    assert_eq!(seen.last().map(Vec::as_slice), Some(&b"counter2"[..]));
    counter
}

/// T8: a scan serves a key the transaction already locked through
/// `get_for_update` at the lock horizon, as `get` does.
#[test]
fn a_scan_serves_a_locked_key_at_its_lock_horizon() {
    for level in levels() {
        let dir = TempDir::new().unwrap();
        let db = pes_db(&dir);
        db.db().put(b"counter", &5u64.to_le_bytes()).unwrap();
        seed(db.db(), &[b"count", b"counter2"]);
        let a = db.begin(&TxnOptions::new().isolation(level));
        let b = db.begin(&TxnOptions::new().isolation(level));
        b.get_for_update(b"counter").unwrap();
        b.put(b"counter", &6u64.to_le_bytes()).unwrap();
        b.commit().unwrap();

        let locked = counter_of(&a.get_for_update(b"counter").unwrap().unwrap());
        assert_eq!(locked, 6);
        let scanned = scan_counter_value(&a).unwrap();
        assert_eq!(
            scanned, locked,
            "{level:?}: the scan must serve the lock horizon"
        );
        a.put(b"counter", &(scanned + 1).to_le_bytes()).unwrap();
        a.commit().unwrap_or_else(|e| panic!("{level:?}: {e}"));
        assert_eq!(counter_of(&db.db().get(b"counter").unwrap().unwrap()), 7);
    }
}

/// T8b: a scan at the begin snapshot before `get_for_update` keeps its anchor.
#[test]
fn a_scan_before_get_for_update_still_anchors_at_the_begin_snapshot() {
    for level in levels() {
        let dir = TempDir::new().unwrap();
        let db = pes_db(&dir);
        db.db().put(b"counter", &5u64.to_le_bytes()).unwrap();
        seed(db.db(), &[b"count", b"counter2"]);
        let a = db.begin(&TxnOptions::new().isolation(level));
        assert_eq!(scan_counter_value(&a), Some(5));
        let b = db.begin(&TxnOptions::new().isolation(level));
        b.get_for_update(b"counter").unwrap();
        b.put(b"counter", &6u64.to_le_bytes()).unwrap();
        b.commit().unwrap();

        assert_eq!(
            counter_of(&a.get_for_update(b"counter").unwrap().unwrap()),
            6
        );
        assert_eq!(scan_counter_value(&a), Some(6));
        a.put(b"counter", &7u64.to_le_bytes()).unwrap();
        assert!(
            is_conflict(&a.commit()),
            "{level:?}: a read 5 before b wrote 6"
        );
    }
}

/// T8c: a locked key deleted before the lock is not yielded.
#[test]
fn a_scan_skips_a_locked_key_deleted_before_the_lock() {
    for level in levels() {
        let dir = TempDir::new().unwrap();
        let db = pes_db(&dir);
        seed(db.db(), &[b"a", b"k", b"z"]);
        let a = db.begin(&TxnOptions::new().isolation(level));
        let b = db.begin(&TxnOptions::new().isolation(level));
        b.get_for_update(b"k").unwrap();
        b.delete(b"k").unwrap();
        b.commit().unwrap();

        assert_eq!(a.get_for_update(b"k").unwrap(), None);
        let stream = a.scan_stream(Some(b"a"), Some(b"zz"));
        let seen: Vec<Vec<u8>> = stream.map(|item| item.unwrap().0).collect();
        assert_eq!(seen, [b"a".to_vec(), b"z".to_vec()], "{level:?}");
    }
}

/// T9: a write inside a walked range is a read; a write the walk passed as the transaction's own entry is not.
#[test]
fn a_write_inside_a_walked_range_is_validated_as_a_read() {
    for level in levels() {
        let dir = TempDir::new().unwrap();
        let db = pes_db(&dir);
        seed(db.db(), &[b"a", b"z"]);
        let a = db.begin(&TxnOptions::new().isolation(level));
        assert_eq!(
            scanned_keys(a.scan_stream(Some(b"a"), Some(b"zz"))),
            [b"a".to_vec(), b"z".to_vec()]
        );
        let b = db.begin(&TxnOptions::new().isolation(level));
        b.put(b"m", b"b").unwrap();
        b.commit().unwrap();
        a.put(b"m", b"a").unwrap();
        assert!(
            is_conflict(&a.commit()),
            "{level:?}: a saw m absent before b wrote it"
        );
    }

    for level in levels() {
        for pessimistic in [false, true] {
            for seeded_m in [false, true] {
                let dir = TempDir::new().unwrap();
                let (opt, pes);
                let (base, a) = if pessimistic {
                    pes = pes_db(&dir);
                    seed(pes.db(), &[b"a", b"z"]);
                    if seeded_m {
                        seed(pes.db(), &[b"m"]);
                    }
                    (pes.db(), pes.begin(&TxnOptions::new().isolation(level)))
                } else {
                    opt = opt_db(&dir);
                    seed(opt.db(), &[b"a", b"z"]);
                    if seeded_m {
                        seed(opt.db(), &[b"m"]);
                    }
                    (opt.db(), opt.begin(&TxnOptions::new().isolation(level)))
                };
                base.put(b"m", b"x").unwrap();
                a.put(b"m", b"x").unwrap();
                assert_eq!(
                    scanned_keys(a.scan_stream(Some(b"a"), Some(b"zz"))),
                    [b"a".to_vec(), b"m".to_vec(), b"z".to_vec()]
                );
                a.commit().unwrap_or_else(|e| {
                    panic!("{level:?} pessimistic={pessimistic} seeded_m={seeded_m}: {e}")
                });
            }
        }
    }
}

/// T10: a stream records its stretch at its first snapshot key and closes it where it stopped.
#[test]
fn a_stream_records_what_it_yielded_whenever_it_stops() {
    let attempt = |scan: &dyn Fn(&regolith::Transaction), key: &[u8]| {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"a", b"b", b"c"]);
        let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
        scan(&a);
        db.db().put(key, b"x").unwrap();
        a.put(key, b"x").unwrap();
        a.commit()
    };
    attempt(&|a| drop(a.scan_stream(None, None)), b"a")
        .expect("a stream that yields nothing records nothing");
    attempt(
        &|a| assert_eq!(a.scan_stream(None, None).take(1).count(), 1),
        b"c",
    )
    .expect("forward take(1) covers a only");
    attempt(
        &|a| {
            assert_eq!(
                a.scan_stream_in(None, None, ScanDirection::Reverse)
                    .take(1)
                    .count(),
                1
            )
        },
        b"a",
    )
    .expect("reverse take(1) covers c only");
    assert!(is_conflict(&attempt(
        &|a| assert_eq!(a.scan_stream(None, None).take(2).count(), 2),
        b"b"
    )));
    assert!(
        is_conflict(&attempt(
            &|a| {
                let mut stream = a.scan_stream(None, None);
                assert!(stream.next().is_some());
                std::mem::forget(stream);
            },
            b"c"
        )),
        "a stretch never closed reaches the end of the keyspace"
    );
}

/// T11: a stream left in scope does not hold its transaction past its last use.
#[test]
fn a_stream_left_in_scope_does_not_hold_the_transaction() {
    let dir = TempDir::new().unwrap();
    let db = opt_db(&dir);
    seed(db.db(), &[b"a"]);
    let tx = db.begin(&TxnOptions::new());
    let mut stream = tx.scan_stream(None, None);
    assert!(stream.next().is_some());
    tx.commit().unwrap();
}

/// W1: at RepeatableRead a scanned key is not validated. The shape that motivates
/// the level: a scan yields a superseded entry and the marker against it, a
/// concurrent reclamation deletes both, and the scanning transaction, which
/// writes only keys of its own, commits. The same schedule at Serializable
/// aborts, pinned here so the difference between the two levels is exactly
/// the one this test names. Both flavours.
///
/// Fails under: `validates_scanned_keys` returning true for RepeatableRead (the
/// RepeatableRead half aborts) or false for Serializable (the Serializable half
/// commits).
#[test]
fn repeatable_read_commits_when_a_scanned_key_is_reclaimed_underneath() {
    let keys: [&[u8]; 3] = [b"head/current", b"head/old", b"marker/old/current"];
    for (level, reclaimed_commits) in [
        (IsolationLevel::RepeatableRead, true),
        (IsolationLevel::Serializable, false),
    ] {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &keys);

        let append = db.begin(&TxnOptions::new().isolation(level));
        let heads = scanned_keys(append.scan_stream(Some(b"head/"), Some(b"head0")));
        let markers = scanned_keys(append.scan_stream(Some(b"marker/"), Some(b"marker0")));
        assert_eq!(heads, [b"head/current".to_vec(), b"head/old".to_vec()]);
        assert_eq!(markers, [b"marker/old/current".to_vec()]);

        let prune = db.begin(&TxnOptions::new().isolation(level));
        prune.delete(b"head/old").unwrap();
        prune.delete(b"marker/old/current").unwrap();
        prune.commit().unwrap();

        append.put(b"head/new", b"0").unwrap();
        append.put(b"marker/current/new", b"0").unwrap();
        let result = append.commit();
        assert_eq!(
            common::committed(&result),
            reclaimed_commits,
            "{level:?}: optimistic append under a reclamation: {result:?}"
        );
    }

    for (level, reclaimed_commits) in [
        (IsolationLevel::RepeatableRead, true),
        (IsolationLevel::Serializable, false),
    ] {
        let dir = TempDir::new().unwrap();
        let db = pes_db(&dir);
        seed(db.db(), &keys);

        let append = db.begin(&TxnOptions::new().isolation(level));
        let heads = scanned_keys(append.scan_stream(Some(b"head/"), Some(b"head0")));
        assert_eq!(heads, [b"head/current".to_vec(), b"head/old".to_vec()]);

        let prune = db.begin(&TxnOptions::new().isolation(level));
        prune.delete(b"head/old").unwrap();
        prune.delete(b"marker/old/current").unwrap();
        prune.commit().unwrap();

        append.put(b"head/new", b"0").unwrap();
        let result = append.commit();
        assert_eq!(
            common::committed(&result),
            reclaimed_commits,
            "{level:?}: pessimistic append under a reclamation: {result:?}"
        );
    }
}

/// W2: RepeatableRead keeps the point-read half of Serializable. A key read
/// through `get` and overwritten by a concurrent commit aborts the
/// transaction, exactly as it does one level up. Both flavours: a
/// pessimistic `get` takes no lock, so the writer commits through the lock
/// manager and only the commit-time check can refuse the reader.
///
/// Fails under: `validates_every_read` returning false for RepeatableRead.
#[test]
fn repeatable_read_aborts_when_a_point_read_key_is_overwritten() {
    let dir = TempDir::new().unwrap();
    let db = opt_db(&dir);
    seed(db.db(), &[b"k"]);

    let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    assert_eq!(a.get(b"k").unwrap(), Some(b"0".to_vec()));
    db.db().put(b"k", b"1").unwrap();
    a.put(b"other", b"1").unwrap();

    match a.commit() {
        Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"k"),
        other => panic!("expected a conflict on k, got {other:?}"),
    }

    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    seed(db.db(), &[b"k"]);

    let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    assert_eq!(a.get(b"k").unwrap(), Some(b"0".to_vec()));
    let writer = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    writer.put(b"k", b"1").unwrap();
    writer.commit().unwrap();
    a.put(b"other", b"1").unwrap();

    match a.commit() {
        Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"k"),
        other => panic!("pessimistic: expected a conflict on k, got {other:?}"),
    }
}

/// W3: what RepeatableRead gives up, stated so it cannot be mistaken for
/// Serializable. Two transactions that each scan the key the other writes
/// both commit: the write skew a per-key scan record refuses one level up.
///
/// Fails under: `validates_scanned_keys` returning true for RepeatableRead.
#[test]
fn repeatable_read_admits_write_skew_through_scans() {
    for (level, both_commit) in [
        (IsolationLevel::RepeatableRead, true),
        (IsolationLevel::Serializable, false),
    ] {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        seed(db.db(), &[b"x", b"y"]);

        let a = db.begin(&TxnOptions::new().isolation(level));
        let b = db.begin(&TxnOptions::new().isolation(level));
        assert_eq!(
            scanned_keys(a.scan_stream(Some(b"y"), Some(b"y0"))),
            [b"y".to_vec()]
        );
        assert_eq!(
            scanned_keys(b.scan_stream(Some(b"x"), Some(b"x0"))),
            [b"x".to_vec()]
        );
        a.put(b"x", b"a").unwrap();
        b.put(b"y", b"b").unwrap();
        a.commit().unwrap();
        assert_eq!(
            common::committed(&b.commit()),
            both_commit,
            "{level:?}: the second of two scan-then-write-the-other transactions"
        );
    }
}

/// W4: a scan at RepeatableRead records exactly what a scan at SnapshotIsolation
/// records. The anchor survives: a key inside the walked stretch that the
/// transaction then writes is validated from the begin snapshot and a key
/// outside it is not, so a bound the walk stopped on is free to change.
///
/// Fails under: RepeatableRead taking the Serializable branch in `yield_cursor`
/// (the bound key `k` is then recorded and the commit aborts).
#[test]
fn repeatable_read_ignores_keys_outside_the_scanned_range() {
    let dir = TempDir::new().unwrap();
    let db = opt_db(&dir);
    seed(db.db(), &[b"a", b"k", b"z"]);

    let a = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    let seen = scanned_keys(a.scan_stream(Some(b"a"), Some(b"k")));
    assert_eq!(seen, [b"a".to_vec()]);

    db.db().put(b"k", b"1").unwrap();
    a.put(b"other", b"1").unwrap();
    a.commit()
        .expect("k was never yielded, so nothing about it is validated");
}
