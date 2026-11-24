//! Property tests for durability, recovery and key ordering.
//!
//! `tests/proptest_invariants.rs` already property-tests a healthy
//! database: scans, snapshots, compaction and a clean reopen, all over
//! plain puts and deletes. This file extends that technique into the two
//! areas it does not reach.
//!
//! 1. **Model-based equivalence over the full write API.** A random
//!    sequence of puts, deletes, range deletes, merge operands and atomic
//!    `WriteBatch`es is applied to regolith and to a `BTreeMap` reference
//!    model, and the two are compared after *every* operation. A single
//!    disagreement anywhere in the sequence fails the case, so this is
//!    the strongest single test in the file.
//! 2. **Durability.** The same generated sequence is run in a child
//!    process that is killed part way through, the unsynced bytes are
//!    then thrown away with the `LD_PRELOAD` power-loss harness in
//!    `tests/common/fault.rs`, and the recovered database must equal the
//!    model after some prefix of the sequence.
//! 3. **Byte transparency and ordering under adversarial keys.** Keys
//!    and values that are empty, all zero, all `0xff`, or that end in
//!    the nine-byte trailer regolith appends to a user key internally, must
//!    round-trip exactly and must iterate in strict user-key order in
//!    both directions.
//!
//! The reference model, the seeded workload generator the child process
//! replays, and the failure-reporting helpers live in the `model`
//! submodule; this file holds the strategies and the properties.
//!
//! # Known failure
//!
//! Two tests here fail against the engine as it stands, and both fail
//! for the same reason:
//! `reverse_iteration_reaches_a_key_above_the_seek_to_last_probe` is the
//! deterministic reproducer, and
//! `iteration_is_in_strict_user_key_order` is the property test that
//! found it. `CfIter::seek_to_last` probes with
//! `cf_upper_bound - 1 || [0xff; 8]`, which is an upper bound only for
//! user keys of at most eight bytes, so any longer key beginning with
//! eight `0xff` bytes is unreachable by a backward walk even though
//! `get` and a forward scan both return it. They are left failing on
//! purpose: a test that found a bug is doing its job.
//!
//! # The property, stated precisely
//!
//! "No data loss" is the wrong bar. Under `DurabilityMode::Eventual`
//! (the default) recent writes are *allowed* to vanish. What is never
//! allowed is a state that is not reachable by applying some prefix of
//! the operations: no gap, no half-applied batch, no key the workload
//! never wrote, no broken iteration order. Under
//! `DurabilityMode::Immediate` the prefix must additionally cover every
//! operation that returned `Ok`.
//!
//! A `kill -9` on its own proves much less than it looks like it does,
//! because every byte the process wrote is still in the OS page cache
//! and the kernel goes on to write it out. The crash test below
//! therefore always follows the kill with
//! [`common::fault::simulate_power_loss`], which discards the bytes that
//! were never `fsync`ed.
//!
//! # Reproducing a failure
//!
//! `proptest` shrinks a failing case, prints the minimal failing input
//! and records the seed in a `.proptest-regressions` file beside this
//! one, which is replayed ahead of any new case on the next run. It
//! prints a notice first that it could not find a `lib.rs` or `main.rs`
//! for its preferred `SourceParallel` layout, then falls back to that
//! sibling file; `proptest_invariants` has one checked in for the same
//! reason. The crash test additionally carries an explicit `u64` seed in
//! its generated input and prints it in every failure message: the whole
//! operation sequence, the durability mode and the crash point are
//! regenerated from that seed and the operation count alone, in the
//! parent and in the child alike.
//!
//! ```sh
//! cargo test --test proptest_durability
//! PROPTEST_CASES=512 cargo test --test proptest_durability
//! ```

// The whole file drives the `LD_PRELOAD` syscall shim, which exists only
// on Linux: macOS blocks the loader equivalent under system integrity
// protection and Windows has none. Gated at the file level, like the
// other shim-backed suites, so the tests are absent where the substrate
// cannot run rather than failing there.
#![cfg(target_os = "linux")]

mod common;
// A test crate's submodules resolve against `tests/`, and every
// `tests/*.rs` is its own test binary, so the model cannot simply live
// beside this file. The directory holds no `main.rs`, so cargo does not
// pick it up as a target of its own.
#[path = "proptest_durability/model.rs"]
mod model;

use std::time::Duration;

use common::fault::{self, ChildSpec, CrashRun, CutPoint, Phase, Trigger};
use model::{
    CRASH_PHASE, CRASH_VALUE_LEN, KEYS, Model, Op, SUFFIX_LOOKALIKE, Unit, WRITE_BUFFER,
    apply_to_db, apply_to_model, closest_prefix, first_difference, kill_after, matching_prefixes,
    model_state, ops_from_seed, opts, show,
};
use proptest::prelude::*;
use regolith::{Db, DurabilityMode};
use tempfile::TempDir;

/// Child process entry point, re-executed by the crash harness. Returns
/// immediately in a normal `cargo test` run.
#[test]
fn crash_child() {
    fault::child_entrypoint(workload);
}

// ── strategies ─────────────────────────────────────────────────

fn pool_key() -> impl Strategy<Value = Vec<u8>> {
    prop::sample::select(KEYS).prop_map(<[u8]>::to_vec)
}

/// Values long enough that a sequence of a few dozen writes overflows
/// the 4 KiB write buffer and forces real flushes.
fn payload() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=200)
}

fn unit() -> impl Strategy<Value = Unit> {
    prop_oneof![
        6 => (pool_key(), payload()).prop_map(|(k, v)| Unit::Put(k, v)),
        2 => pool_key().prop_map(Unit::Delete),
        2 => (pool_key(), pool_key()).prop_map(|(a, b)| {
            let (s, e) = if a <= b { (a, b) } else { (b, a) };
            Unit::DeleteRange(s, e)
        }),
        2 => (pool_key(), prop::collection::vec(any::<u8>(), 1..=6))
            .prop_map(|(k, o)| Unit::Merge(k, o)),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        7 => unit().prop_map(Op::Single),
        3 => prop::collection::vec(unit(), 2..=5).prop_map(Op::Batch),
        1 => Just(Op::Compact),
    ]
}

fn ops(len: std::ops::RangeInclusive<usize>) -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op(), len)
}

/// Keys built to collide with each other: a tiny alphabet gives many
/// prefix relations, and one arm appends [`SUFFIX_LOOKALIKE`].
fn adversarial_key() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => prop::collection::vec(prop::sample::select(&[0u8, 1, b'a', b'b', 0xfe, 0xff][..]), 0..=6),
        1 => prop::collection::vec(any::<u8>(), 0..=4)
            .prop_map(|mut k| { k.extend_from_slice(&SUFFIX_LOOKALIKE); k }),
        1 => prop::collection::vec(any::<u8>(), 0..=12),
    ]
}

/// Values covering the byte patterns most likely to break a length
/// prefix, a checksum or a block encoder: empty, all zero, all `0xff`,
/// an internal-key suffix lookalike, and arbitrary bytes.
fn adversarial_value() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        1 => Just(Vec::new()),
        1 => (0usize..=64).prop_map(|n| vec![0u8; n]),
        1 => (0usize..=64).prop_map(|n| vec![0xffu8; n]),
        1 => Just(SUFFIX_LOOKALIKE.to_vec()),
        4 => prop::collection::vec(any::<u8>(), 0..=128),
    ]
}

/// Compare a live database against the model through both read paths:
/// the range scan and a point lookup of every key in the pool, including
/// the keys the model says are absent.
fn compare(db: &Db, model: &Model) -> Result<(), TestCaseError> {
    let got = db
        .scan(None, None)
        .map_err(|e| TestCaseError::fail(format!("scan failed: {e}")))?;
    let want = model_state(model);
    if let Some(diff) = first_difference(&got, &want) {
        return Err(TestCaseError::fail(format!(
            "scan disagrees with the model: {diff}"
        )));
    }
    for key in KEYS {
        let got = db
            .get(key)
            .map_err(|e| TestCaseError::fail(format!("get failed: {e}")))?;
        let want = model.get(*key).cloned();
        if got != want {
            return Err(TestCaseError::fail(format!(
                "get({}) returned {:?}, model says {:?}",
                show(key),
                got.as_deref().map(show),
                want.as_deref().map(show),
            )));
        }
    }
    Ok(())
}

// ── property tests ─────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Proves regolith and a `BTreeMap` reference model agree after *every*
    /// operation of a random sequence of puts, deletes, range deletes,
    /// merge operands, atomic batches and compactions, through both the
    /// scan path and the point-lookup path.
    ///
    /// Catches: any divergence between the write path and the read path
    /// that a hand-written test did not think to sequence. Concretely, a
    /// range tombstone that fails to shadow an older put, a merge chain
    /// that collapses in the wrong order or picks up a base value it
    /// should not see, a batch whose operations are reordered, a delete
    /// that does not survive a flush, and any disagreement between
    /// `scan` and `get`. The comparison runs after every operation, so
    /// the reported failure is the first one, not the last.
    #[test]
    fn regolith_agrees_with_the_model_after_every_operation(sequence in ops(1..=60)) {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual)).unwrap();
        let mut model = Model::new();

        for (i, op) in sequence.iter().enumerate() {
            apply_to_db(&db, op)
                .map_err(|e| TestCaseError::fail(format!("op {i} ({op:?}) failed: {e}")))?;
            apply_to_model(&mut model, op);
            compare(&db, &model)
                .map_err(|e| TestCaseError::fail(format!("after op {i} ({op:?}): {e}")))?;
        }
    }

    /// Proves an arbitrary operation sequence survives a reopen exactly:
    /// the database is dropped without `close`, so recovery has to
    /// replay the write-ahead log, including the range tombstones and
    /// merge operands the log carries as their own record types.
    ///
    /// Catches: a WAL record type that is written but not replayed (a
    /// range delete or a merge operand silently degrading into nothing
    /// on restart), replay that applies a batch out of order, and a
    /// flush-then-replay combination that resurrects a deleted key. The
    /// second phase compacts after recovery, so it also catches a
    /// recovered range tombstone that fails to shadow older data once it
    /// reaches an SSTable.
    #[test]
    fn an_operation_sequence_survives_a_reopen_exactly(sequence in ops(1..=40)) {
        let dir = TempDir::new().unwrap();
        let mut model = Model::new();
        {
            let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual)).unwrap();
            for op in &sequence {
                apply_to_db(&db, op).map_err(|e| TestCaseError::fail(e.to_string()))?;
                apply_to_model(&mut model, op);
            }
        }

        let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual))
            .map_err(|e| TestCaseError::fail(format!("reopen after drop failed: {e}")))?;
        compare(&db, &model).map_err(|e| TestCaseError::fail(format!("after reopen: {e}")))?;
        db.compact_range(None, None).wait().unwrap();
        compare(&db, &model)
            .map_err(|e| TestCaseError::fail(format!("after reopen and compaction: {e}")))?;
    }

    /// Proves that a snapshot taken at sequence number S sees exactly
    /// the writes with sequence number <= S, for an arbitrary
    /// interleaving of operations and snapshots, and that it keeps
    /// seeing them after a full compaction runs underneath it.
    ///
    /// `proptest_invariants::snapshot_isolation` checks one snapshot
    /// with point lookups only. This checks many overlapping snapshots
    /// through the scan path as well, and it holds them open across a
    /// compaction, which is where the versions a snapshot depends on can
    /// actually be dropped.
    ///
    /// Catches: a snapshot read that leaks a later write, a compaction
    /// that garbage-collects a version a live snapshot still pins (the
    /// failure mode where an old snapshot starts returning today's
    /// data), and a range tombstone or merge operand applied at the
    /// wrong visibility boundary.
    #[test]
    fn snapshots_see_exactly_the_writes_that_preceded_them(sequence in ops(4..=40)) {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual)).unwrap();
        let mut model = Model::new();
        let mut pinned = Vec::new();

        for (i, op) in sequence.iter().enumerate() {
            apply_to_db(&db, op).map_err(|e| TestCaseError::fail(e.to_string()))?;
            apply_to_model(&mut model, op);
            if i % 3 == 1 {
                pinned.push((db.snapshot(), model.clone(), i));
            }
        }
        db.compact_range(None, None).wait().unwrap();

        for (snap, want, i) in &pinned {
            let got = snap
                .scan(None, None)
                .map_err(|e| TestCaseError::fail(format!("snapshot scan failed: {e}")))?;
            if let Some(diff) = first_difference(&got, &model_state(want)) {
                return Err(TestCaseError::fail(format!(
                    "snapshot taken after op {i} disagrees with the model as of that op: {diff}"
                )));
            }
            for key in KEYS {
                let got = snap
                    .get(key)
                    .map_err(|e| TestCaseError::fail(format!("snapshot get failed: {e}")))?;
                prop_assert_eq!(
                    got.as_deref().map(show),
                    want.get(*key).map(|v| show(v.as_slice())),
                    "snapshot taken after op {} disagrees on key {}",
                    i,
                    show(key)
                );
            }
        }
        compare(&db, &model)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Proves any key and any value round-trips byte for byte: empty,
    /// all zero, all `0xff`, and byte strings ending in the exact
    /// nine-byte `!seq || value_type` trailer that regolith appends to a
    /// user key internally. Verified in the memtable, after a WAL replay
    /// on reopen, and after compaction has rewritten every key through
    /// the block encoder.
    ///
    /// Catches: an internal key split by scanning for the trailer rather
    /// than by length, a block encoder whose prefix compression
    /// mis-restores a shared prefix, a length prefix that cannot express
    /// an empty value, and any confusion between "value is empty" and
    /// "key is absent" across a restart.
    #[test]
    fn arbitrary_keys_and_values_round_trip_exactly(
        entries in prop::collection::vec((adversarial_key(), adversarial_value()), 1..=60),
    ) {
        let dir = TempDir::new().unwrap();
        let mut model = Model::new();
        {
            let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual)).unwrap();
            for (k, v) in &entries {
                regolith::through_stalls(|| db.put(k, v)).map_err(|e| TestCaseError::fail(e.to_string()))?;
                model.insert(k.clone(), v.clone());
                let got = db.get(k).map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(
                    got.as_deref().map(show),
                    Some(show(v)),
                    "key {} did not read back the value just written",
                    show(k)
                );
            }
        }

        let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual))
            .map_err(|e| TestCaseError::fail(format!("reopen failed: {e}")))?;
        compare_scan_only(&db, &model, "after reopen")?;
        db.compact_range(None, None).wait().unwrap();
        compare_scan_only(&db, &model, "after compaction")?;
    }

    /// Proves iteration is in strict ascending user-key order for
    /// arbitrary key sets, that reverse iteration is the exact mirror of
    /// forward iteration, and that `seek` and `seek_for_prev` land on
    /// the first key `>= target` and the last key `<= target`. Checked
    /// on the memtable and again after compaction, because the ordering
    /// is enforced by different code in each: the skip list's `Ord` and
    /// the SSTable comparator.
    ///
    /// The key sets are built to hammer the one case the internal-key
    /// comparator exists for: user keys where one is a prefix of
    /// another, so the shorter key's `!seq` trailer lines up against the
    /// longer key's data bytes.
    ///
    /// Catches: a comparison site that compares encoded internal keys as
    /// raw bytes (which reverses the order of `a` and `a\x00`), an
    /// SSTable index whose block boundaries disagree with the block
    /// contents, and a `prev` path that skips or repeats an entry.
    ///
    /// This was red before `seek_to_last` took an exclusive upper
    /// bound: it shrank to the single key `ff ff ff ff ff ff ff ff 01`,
    /// which a forward scan returned and a backward walk could not
    /// reach. See
    /// `reverse_iteration_reaches_a_key_above_the_seek_to_last_probe`
    /// below for the deterministic form and the diagnosis.
    #[test]
    fn iteration_is_in_strict_user_key_order(
        keys in prop::collection::vec(adversarial_key(), 1..=64),
    ) {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual)).unwrap();

        let mut sorted: Vec<Vec<u8>> = keys;
        sorted.sort();
        sorted.dedup();
        for k in &sorted {
            regolith::through_stalls(|| db.put(k, b"v")).map_err(|e| TestCaseError::fail(e.to_string()))?;
        }

        // A probe per key would make the case quadratic in iterator
        // constructions; a stride keeps it linear while still covering
        // the whole key space, the gaps between keys and both ends.
        let stride = (sorted.len() / 8).max(1);
        let mut probes: Vec<Vec<u8>> = vec![Vec::new(), vec![0xff]];
        for k in sorted.iter().step_by(stride) {
            probes.push(k.clone());
            let mut longer = k.clone();
            longer.push(0);
            probes.push(longer);
            if !k.is_empty() {
                probes.push(k[..k.len() - 1].to_vec());
            }
        }

        check_order(&db, &sorted, &probes, "in the memtable")?;
        db.compact_range(None, None).wait().unwrap();
        check_order(&db, &sorted, &probes, "after compaction")?;
    }
}

/// Scan-only comparison, for the round-trip test where the key space is
/// arbitrary rather than the fixed pool.
fn compare_scan_only(db: &Db, model: &Model, when: &str) -> Result<(), TestCaseError> {
    let got = db
        .scan(None, None)
        .map_err(|e| TestCaseError::fail(format!("{when}: scan failed: {e}")))?;
    if let Some(diff) = first_difference(&got, &model_state(model)) {
        return Err(TestCaseError::fail(format!("{when}: {diff}")));
    }
    for (k, v) in model {
        let got = db
            .get(k)
            .map_err(|e| TestCaseError::fail(format!("{when}: get failed: {e}")))?;
        if got.as_deref() != Some(v.as_slice()) {
            return Err(TestCaseError::fail(format!(
                "{when}: get({}) returned {:?}, expected {}",
                show(k),
                got.as_deref().map(show),
                show(v),
            )));
        }
    }
    Ok(())
}

fn check_order(
    db: &Db,
    sorted: &[Vec<u8>],
    probes: &[Vec<u8>],
    when: &str,
) -> Result<(), TestCaseError> {
    let mut forward = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        forward.push(it.key().expect("valid iterator has a key").to_vec());
        it.next();
    }
    for pair in forward.windows(2) {
        if pair[0] >= pair[1] {
            return Err(TestCaseError::fail(format!(
                "{when}: forward iteration is not strictly ascending: {} then {}",
                show(&pair[0]),
                show(&pair[1]),
            )));
        }
    }
    if forward != sorted {
        let divergence = forward
            .iter()
            .zip(sorted.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(forward.len().min(sorted.len()));
        return Err(TestCaseError::fail(format!(
            "{when}: forward iteration yielded {} keys, the key set holds {}; first divergence \
             at index {}: iterator says {:?}, the key set says {:?}",
            forward.len(),
            sorted.len(),
            divergence,
            forward.get(divergence).map(|k| show(k)),
            sorted.get(divergence).map(|k| show(k)),
        )));
    }

    let mut backward = Vec::new();
    let mut rit = db.iter();
    rit.seek_to_last();
    while rit.valid() {
        backward.push(rit.key().expect("valid iterator has a key").to_vec());
        rit.prev();
    }
    backward.reverse();
    if backward != forward {
        return Err(TestCaseError::fail(format!(
            "{when}: reverse iteration yielded {} keys, forward yielded {}",
            backward.len(),
            forward.len(),
        )));
    }

    for probe in probes {
        let mut it = db.iter();
        it.seek(probe);
        let got = it.key().map(<[u8]>::to_vec);
        let want = sorted.iter().find(|k| *k >= probe).cloned();
        if got != want {
            return Err(TestCaseError::fail(format!(
                "{when}: seek({}) landed on {:?}, first key >= it is {:?}",
                show(probe),
                got.as_deref().map(show),
                want.as_deref().map(show),
            )));
        }

        let mut it = db.iter();
        it.seek_for_prev(probe);
        let got = it.key().map(<[u8]>::to_vec);
        let want = sorted.iter().rev().find(|k| *k <= probe).cloned();
        if got != want {
            return Err(TestCaseError::fail(format!(
                "{when}: seek_for_prev({}) landed on {:?}, last key <= it is {:?}",
                show(probe),
                got.as_deref().map(show),
                want.as_deref().map(show),
            )));
        }
    }
    Ok(())
}

/// Proves reverse iteration reaches every key a forward scan returns,
/// for the one key shape whose failure the property test above shrank
/// to. Deterministic, so it stays a reproducer no matter what the
/// property test's RNG does next.
///
/// `CfIter::seek_to_last` used to position the cursor with
/// `seek_for_prev(cf_upper_bound - 1 || [0xff; 8])`, a probe that is an
/// upper bound only for user keys of at most eight bytes. The default
/// column family is id 1, so the probe was
/// `00 00 00 01 || ff ff ff ff ff ff ff ff`, while the key below
/// encodes as `00 00 00 01 || ff ff ff ff ff ff ff ff || 01` and sorts
/// *above* it: `seek_to_last` landed before the key and the reverse
/// walk missed it entirely. No suffix of `0xff` bytes fixes that, since
/// byte strings have no predecessor, so `seek_to_last` now takes the
/// exclusive bound directly (`RegolithIterator::seek_to_last_before`).
///
/// Catches: exactly that. The key is written, `get` returns it and a
/// forward scan returns it; only the backward walk dropped it, and the
/// same probe is used by `Snapshot::iter` and the tailing iterator.
#[test]
fn reverse_iteration_reaches_a_key_above_the_seek_to_last_probe() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual)).unwrap();
    let key = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
    regolith::through_stalls(|| db.put(&key, b"v")).unwrap();

    assert_eq!(db.get(&key).unwrap(), Some(b"v".to_vec()));
    assert_eq!(
        db.scan(None, None).unwrap(),
        vec![(key.to_vec(), b"v".to_vec())],
        "a forward scan must return the key",
    );

    let mut it = db.iter();
    it.seek_to_last();
    assert!(
        it.valid(),
        "seek_to_last() found nothing in a database holding one key, {}. The probe it seeks \
         with is only an upper bound for user keys of eight bytes or fewer, so every longer \
         key starting with eight 0xff bytes is unreachable backwards.",
        show(&key),
    );
    assert_eq!(it.key(), Some(&key[..]));
}

/// A full backward walk over the two keys that sit either side of the
/// old probe returns the exact reverse of the forward walk.
///
/// `ff*8 || 00` sorted below the old probe and `ff*8 || 01` above it,
/// so the pair pins both halves of the boundary: a reverse walk that
/// starts too low loses the second key, and one that starts too high
/// loses the first.
#[test]
fn reverse_iteration_over_the_probe_boundary_mirrors_the_forward_walk() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts(WRITE_BUFFER, DurabilityMode::Eventual)).unwrap();
    let low = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
    let high = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
    regolith::through_stalls(|| db.put(&low, b"l")).unwrap();
    regolith::through_stalls(|| db.put(&high, b"h")).unwrap();

    let mut forward = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        forward.push(it.key().unwrap().to_vec());
        it.next();
    }
    it.status().unwrap();
    assert_eq!(forward, vec![low.to_vec(), high.to_vec()]);

    let mut backward = Vec::new();
    let mut it = db.iter();
    it.seek_to_last();
    while it.valid() {
        backward.push(it.key().unwrap().to_vec());
        it.prev();
    }
    it.status().unwrap();
    backward.reverse();
    assert_eq!(
        backward, forward,
        "the reverse walk must mirror the forward one"
    );
}

/// The child-side workload. Applies the regenerated sequence and kills
/// itself the way a power supply would, immediately after the
/// `kill_after`th operation has returned `Ok`, so every operation the
/// parent counts as acknowledged really was acknowledged.
fn workload(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == CRASH_PHASE => {}
        _ => {
            fault::builtin_workload(spec);
            return;
        }
    }

    let db = Db::open(&spec.db_path, opts(spec.write_buffer_size, spec.durability))
        .expect("child: open db");
    let sequence = ops_from_seed(spec.seed, spec.ops, spec.value_len);
    let stop = kill_after(spec.seed, spec.ops);
    for (i, op) in sequence.iter().enumerate() {
        apply_to_db(&db, op).expect("child: write failed");
        if i + 1 == stop {
            fault::kill_self();
        }
    }
    db.close().expect("child: close");
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        max_shrink_iters: 8,
        ..ProptestConfig::default()
    })]

    /// The durability test. Runs a random operation sequence in a real
    /// child process, kills it at a random point, throws away every byte
    /// the child wrote but never `fsync`ed, and reopens.
    ///
    /// Two properties are asserted:
    ///
    /// * In **either** durability mode the recovered state must equal
    ///   the model after some prefix of the sequence that the child
    ///   actually reached. Batches are atomic, so a state that matches
    ///   no prefix means a batch was half applied, a write from the
    ///   middle vanished, or recovery invented one.
    /// * Under `DurabilityMode::Immediate` the surviving prefix must be
    ///   exactly the set of operations that returned `Ok`. The child
    ///   kills itself on the statement after the `kill_after`th write
    ///   returned, so the parent knows that number without trusting the
    ///   process it just killed.
    ///
    /// The power-loss step is what makes this a durability test rather
    /// than a crash test: a `kill -9` leaves the unsynced bytes in the
    /// page cache and the kernel writes them out anyway, so a kill alone
    /// would pass even if `Immediate` never called `fsync`.
    ///
    /// Catches: an `Immediate` write acknowledged before its WAL fsync
    /// returns, recovery that skips a damaged record and applies the
    /// next one (a silent hole), a `WriteBatch` applied operation by
    /// operation as it decodes rather than validated whole, a range
    /// tombstone or merge operand that is not replayed from the WAL, and
    /// a flush whose SSTable is published into the MANIFEST before its
    /// bytes are durable.
    ///
    /// Runtime: measured at 1.1s for the 128 cases configured here, one
    /// child process each (2.5s for 300 cases when the count is raised
    /// by hand). It spawns a process per case and needs the
    /// `LD_PRELOAD` shim, so it is Linux-only: the whole module is
    /// gated on that rather than on the test being switched off.
    #[test]
    fn a_power_cut_at_a_random_point_leaves_a_prefix_of_the_model(
        seed in any::<u64>(),
        count in 60usize..=140,
        immediate in any::<bool>(),
    ) {
        let mode = if immediate {
            DurabilityMode::Immediate
        } else {
            DurabilityMode::Eventual
        };
        let tmp = TempDir::new().unwrap();
        let db_dir = tmp.path().join("db");
        let spec = ChildSpec::new(Phase::Custom(CRASH_PHASE.to_string()), &db_dir)
            .seed(seed)
            .ops(count)
            .value_len(CRASH_VALUE_LEN)
            .write_buffer_size(8 * 1024)
            .durability(mode);
        let out = CrashRun::new(spec)
            .trigger(Trigger::Workload)
            .timeout(Duration::from_secs(120))
            .run();
        out.assert_killed();

        let last_wal_write = out.journal.writes_to("/wal/").last().map(|r| r.seq);
        let last_wal_sync = out.journal.syncs_to("/wal/").last().map(|r| r.seq);
        prop_assert!(
            last_wal_write.is_some(),
            "seed {seed:#x}: the shim recorded no WAL write at all, so the power-loss \
             reconstruction had nothing to work from and this case would prove nothing.\n{}",
            out.journal,
        );

        let report = fault::simulate_power_loss(&db_dir, CutPoint::End);
        // Whether a given case has unsynced bytes to throw away depends
        // on where the kill landed, so this is the implication rather
        // than a flat assertion: when the recording says the last WAL
        // write was never followed by an fsync, the reconstruction must
        // have discarded something. A "power loss" that kept those bytes
        // would be a process kill with extra steps.
        if last_wal_write > last_wal_sync {
            prop_assert!(
                report.discarded_anything(),
                "seed {seed:#x}, {count} ops, {mode:?}: the last WAL write (seq {:?}) was never \
                 fsynced (last WAL fsync seq {:?}), yet the power cut discarded nothing.\n{}",
                last_wal_write,
                last_wal_sync,
                report.summary(),
            );
        }

        let sequence = ops_from_seed(seed, count, CRASH_VALUE_LEN);
        let stop = kill_after(seed, count);

        let db = Db::open(&db_dir, opts(WRITE_BUFFER, mode)).map_err(|e| {
            TestCaseError::fail(format!(
                "seed {seed:#x}, {count} ops, {mode:?}: the database refused to open after a \
                 power cut that only truncated unsynced tails: {e}\n{}",
                report.summary(),
            ))
        })?;
        let state = fault::recovered_state(&db)
            .map_err(|e| TestCaseError::fail(format!("seed {seed:#x}: {e}")))?;

        let prefixes = matching_prefixes(&state, &sequence);
        prop_assert!(
            !prefixes.is_empty(),
            "seed {seed:#x}, {count} ops, {mode:?}, killed after op {stop}: the recovered state \
             matches no prefix of the sequence, so recovery produced something the workload \
             never asked for. It holds {} live key(s). {}\n{}",
            state.len(),
            closest_prefix(&state, &sequence),
            report.summary(),
        );
        prop_assert!(
            prefixes.iter().any(|k| *k <= stop),
            "seed {seed:#x}, {count} ops, {mode:?}: the recovered state only matches prefix(es) \
             {prefixes:?}, all longer than the {stop} operations the child ever issued",
        );
        if immediate {
            prop_assert!(
                prefixes.contains(&stop),
                "seed {seed:#x}, {count} ops, Immediate durability: {stop} operations returned \
                 Ok before the crash, but the recovered state matches only prefix(es) \
                 {prefixes:?}. An acknowledged write did not survive the power cut.\n{}",
                report.summary(),
            );
        }
    }
}
