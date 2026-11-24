//! Validation outside the pipeline (`early_check_split` in
//! `PipelineCheck.lean`): checking a commit up to a horizon `h` before it
//! takes a slot, and only above `h` when its slot is decided, decides as one
//! check of every version does.

use proptest::prelude::*;
use tempfile::TempDir;

use super::super::{ConflictKey, EngineOptions, RangeCheck, ReadRule, ValidationSet};
use super::early::EarlyVerdict;
use super::*;
use crate::Access;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};

/// Keeps the newest operand on top of the base, so operands and bases both
/// show in a value.
struct Concat;

impl crate::MergeOperator for Concat {
    fn name(&self) -> &'static str {
        "concat"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }
}

fn open() -> (TempDir, Arc<RegolithEngine>) {
    open_with(EngineOptions::default())
}

fn open_with(options: EngineOptions) -> (TempDir, Arc<RegolithEngine>) {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            merge_operator: Some(Arc::new(Concat)),
            ..options
        },
    )
    .unwrap();
    (dir, engine)
}

const NAMES: [&[u8]; 4] = [b"a", b"b", b"c", b"d"];

fn key_of(name: &[u8]) -> Vec<u8> {
    prefix_key(DEFAULT_CF_ID, name)
}

/// One step of a history: a write of one key, a range delete, or a flush.
#[derive(Clone, Debug)]
enum Step {
    Put(usize, u8),
    Delete(usize),
    Merge(usize, u8),
    DeleteRange(usize, usize),
    Flush,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (0..NAMES.len(), 0u8..2).prop_map(|(k, v)| Step::Put(k, v)),
        2 => (0..NAMES.len()).prop_map(Step::Delete),
        3 => (0..NAMES.len(), 0u8..2).prop_map(|(k, v)| Step::Merge(k, v)),
        1 => (0..NAMES.len(), 1..=NAMES.len()).prop_map(|(a, b)| Step::DeleteRange(a, b)),
        1 => Just(Step::Flush),
    ]
}

fn apply(engine: &RegolithEngine, step: &Step) {
    let op = match *step {
        Step::Put(k, v) => WriteBatchOp::Put {
            key: key_of(NAMES[k]),
            value: vec![b'0' + v],
        },
        Step::Delete(k) => WriteBatchOp::Delete {
            key: key_of(NAMES[k]),
        },
        Step::Merge(k, v) => WriteBatchOp::Merge {
            key: key_of(NAMES[k]),
            operand: vec![b'm', b'0' + v],
        },
        Step::DeleteRange(a, b) => {
            let (lo, end) = (a.min(b), a.max(b).min(NAMES.len() - 1));
            if lo >= end {
                return;
            }
            WriteBatchOp::DeleteRange {
                start: key_of(NAMES[lo]),
                end: key_of(NAMES[end]),
            }
        }
        Step::Flush => {
            engine.flush_active_memtable().unwrap();
            return;
        }
    };
    engine
        .apply_batch(vec![op], DurabilityMode::Eventual, false)
        .unwrap();
}

/// A generated transaction: what it read and how, what it scanned, what it
/// writes, and which keys a classifier exempted.
#[derive(Clone, Debug)]
struct Shape {
    reads: Vec<(usize, u8)>,
    scan: Option<(usize, usize)>,
    writes: Vec<(usize, Step)>,
    blind: bool,
    exempt: Option<usize>,
}

fn shape() -> impl Strategy<Value = Shape> {
    let write = prop_oneof![
        (0u8..2).prop_map(|v| Step::Put(0, v)),
        Just(Step::Delete(0)),
        (0u8..2).prop_map(|v| Step::Merge(0, v)),
    ];
    (
        proptest::collection::vec((0..NAMES.len(), 0u8..3), 0..3),
        proptest::option::of((0..NAMES.len(), 1..=NAMES.len())),
        proptest::collection::vec((0..NAMES.len(), write), 0..3),
        any::<bool>(),
        proptest::option::of(0..NAMES.len()),
    )
        .prop_map(|(reads, scan, writes, blind, exempt)| Shape {
            reads,
            scan,
            writes,
            blind,
            exempt,
        })
}

/// The commit `shape` makes from a snapshot at `snapshot`.
fn commit_of(shape: &Shape, snapshot: u64) -> (ValidationSet, Vec<WriteBatchOp>) {
    let mut points = std::collections::BTreeMap::new();
    let mut merges = Vec::new();
    for (k, w) in &shape.writes {
        match w {
            Step::Put(_, v) => {
                points.insert(key_of(NAMES[*k]), Some(vec![b'0' + v]));
            }
            Step::Delete(_) => {
                points.insert(key_of(NAMES[*k]), None);
            }
            Step::Merge(_, v) => merges.push((key_of(NAMES[*k]), vec![b'm', b'0' + v])),
            _ => {}
        }
    }
    let exempt: Vec<Vec<u8>> = shape
        .exempt
        .map(|k| key_of(NAMES[k]))
        .filter(|key| {
            // A classifier never exempts a key the commit deletes.
            !matches!(points.get(key), Some(None))
        })
        .into_iter()
        .collect();
    let mut reads: Vec<ConflictKey> = shape
        .reads
        .iter()
        .map(|&(k, how)| ConflictKey {
            key: key_of(NAMES[k]),
            observed_seq: snapshot,
            found: false,
            access: if how == 2 {
                Access::ReadPresence
            } else {
                Access::Read
            },
            rule: if how == 1 {
                ReadRule::Value
            } else {
                ReadRule::Seq
            },
        })
        .filter(|read| !exempt.contains(&read.key))
        .collect();
    reads.sort_by(|a, b| a.key.cmp(&b.key));
    reads.dedup_by(|a, b| a.key == b.key);
    let ranges = shape
        .scan
        .and_then(|(a, b)| {
            let (lo, hi) = (a.min(b), a.max(b));
            (lo < hi).then(|| RangeCheck {
                lo: key_of(NAMES[lo]),
                hi: key_of(NAMES[hi.min(NAMES.len() - 1)]),
                observed_seq: snapshot,
            })
        })
        .filter(|range| range.lo < range.hi)
        .into_iter()
        .collect();
    let checks = ValidationSet {
        reads,
        writes_at: Some(snapshot),
        blind_merges_commute: shape.blind,
        exempt,
        ranges,
    };
    (checks, grouped_batch_ops(points, Vec::new(), merges))
}

/// What a commit decided, in a form both engines report.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Landed {
        seq: Option<u64>,
        commuted: u64,
        elided: u64,
    },
    Lost {
        key: Vec<u8>,
        mine: Access,
        latest: u64,
    },
    Refused(String),
}

/// Check `checks` and `ops` at the current horizon, then run `between` and
/// commit what the check left to the group. Returns the outcome and whether
/// the check at the horizon settled it.
fn split_commit(
    engine: &RegolithEngine,
    checks: ValidationSet,
    ops: Vec<WriteBatchOp>,
    between: impl FnOnce(),
) -> (Outcome, bool) {
    let early = match engine.check_early(&checks, &ops).unwrap() {
        EarlyVerdict::Conflict(conflict) => {
            between();
            return (
                Outcome::Lost {
                    key: conflict.key().to_vec(),
                    mine: conflict.mine(),
                    latest: conflict.latest_seq(),
                },
                true,
            );
        }
        EarlyVerdict::Marks(early) => early,
    };
    between();
    let request = WriteRequest::Txn(Box::new(TxnRequest {
        checks,
        ops,
        appends: Vec::new(),
        durability: DurabilityMode::Eventual,
        perf: crate::PerfLevel::Disable,
        early,
        read_queue: None,
    }));
    let settled = engine
        .commit_through_pipeline(request)
        .map_err(|e| e.to_string());
    let outcome = match settled {
        Ok(Settled::Committed {
            seq,
            merges_commuted,
            writes_elided,
            ..
        }) => Outcome::Landed {
            seq,
            commuted: merges_commuted,
            elided: writes_elided,
        },
        Ok(Settled::Conflict { conflict, .. }) => Outcome::Lost {
            key: conflict.key().to_vec(),
            mine: conflict.mine(),
            latest: conflict.latest_seq(),
        },
        Ok(Settled::Write(_)) => panic!("a transaction settled as a plain write"),
        Err(e) => Outcome::Refused(e),
    };
    (outcome, false)
}

fn read_all(engine: &RegolithEngine) -> Vec<Option<Vec<u8>>> {
    NAMES
        .iter()
        .map(|name| engine.get(&key_of(name), u64::MAX).unwrap())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The commit checked up to a horizon taken partway through the writes
    /// that follow its snapshot, and above it when its slot is decided,
    /// decides as the same commit checked in full once every write has
    /// landed. When the check at the horizon settles the commit, the outcome
    /// is the same and only which conflicting key it names may differ: a
    /// conflict behind a put to a content-addressed key, which a write above
    /// the horizon can turn into a refusal the full check meets first, is
    /// not settled at the horizon.
    #[test]
    fn checking_up_to_a_horizon_then_above_it_decides_as_one_check(
        before in proptest::collection::vec(step(), 0..5),
        until_horizon in proptest::collection::vec(step(), 0..5),
        after_horizon in proptest::collection::vec(step(), 0..5),
        shape in shape(),
    ) {
        let (_split_dir, split) = open();
        let (_whole_dir, whole) = open();
        for engine in [&split, &whole] {
            for s in &before {
                apply(engine, s);
            }
        }
        // Pinned, as a transaction pins the snapshot it validates against:
        // a compaction on the worker keeps every version above it, a
        // deletion included, until both commits are decided.
        let (snapshot, split_pin) = split.register_snapshot_at_horizon();
        let (whole_snapshot, whole_pin) = whole.register_snapshot_at_horizon();
        prop_assert_eq!(snapshot, whole_snapshot);
        for engine in [&split, &whole] {
            for s in &until_horizon {
                apply(engine, s);
            }
        }
        let (checks, ops) = commit_of(&shape, snapshot);
        let (split_outcome, settled_early) =
            split_commit(&split, checks, ops, || {
                for s in &after_horizon {
                    apply(&split, s);
                }
            });
        for s in &after_horizon {
            apply(&whole, s);
        }
        let (checks, ops) = commit_of(&shape, snapshot);
        // Every write has landed: the check at the horizon sees them all and
        // nothing lands above it, so this is one check of every version.
        let (whole_outcome, _) = split_commit(&whole, checks, ops, || {});
        split.release_snapshot(split_pin);
        whole.release_snapshot(whole_pin);
        match (&split_outcome, &whole_outcome) {
            (Outcome::Lost { .. }, Outcome::Lost { .. }) if settled_early => {}
            _ => prop_assert_eq!(&split_outcome, &whole_outcome),
        }
        prop_assert_eq!(read_all(&split), read_all(&whole));
    }
}

/// What made the property test above disagree with itself: it took its
/// snapshot without pinning it, and the engine's worker raced it. A table
/// that holds one deletion and nothing else is mostly dead, so a pass picked
/// for its deletions compacts it; with no snapshot pinned below the deletion
/// that pass retires it, and a check against the unpinned snapshot then
/// finds no newer version of the key. Which engine the worker reached
/// first decided the outcome. A transaction pins its snapshot from `begin`
/// until its commit is decided, and with the pin the deletion stays and the
/// check conflicts. Run here without a worker, one pass at a time.
#[test]
fn a_deletion_newer_than_a_pinned_snapshot_stays_for_the_check() {
    let quiet = || EngineOptions {
        max_background_compactions: 0,
        ..EngineOptions::default()
    };
    let entries = |engine: &RegolithEngine| {
        engine
            .current_version()
            .levels
            .iter()
            .flatten()
            .map(|t| t.meta.num_entries)
            .sum::<u64>()
    };
    let read_of_b = |snapshot| ValidationSet {
        reads: vec![ConflictKey {
            key: key_of(b"b"),
            observed_seq: snapshot,
            found: false,
            access: Access::Read,
            rule: ReadRule::Seq,
        }],
        writes_at: None,
        blind_merges_commute: false,
        exempt: Vec::new(),
        ranges: Vec::new(),
    };

    // Nothing pinned: the pass runs and the deletion leaves the tree.
    let (_dir, engine) = open_with(quiet());
    apply(&engine, &Step::Delete(1));
    apply(&engine, &Step::Flush);
    assert_eq!(entries(&engine), 1);
    assert_eq!(
        engine.run_one_compaction_pass().unwrap(),
        crate::CompactionOutcome::DidWork
    );
    assert_eq!(entries(&engine), 0, "the deletion retired");

    // Pinned below the deletion: no pass is picked for it, a compaction of
    // the whole range keeps it, and the check sees it.
    let (_dir, engine) = open_with(quiet());
    let (snapshot, pin) = engine.register_snapshot_at_horizon();
    apply(&engine, &Step::Delete(1));
    apply(&engine, &Step::Flush);
    assert_eq!(
        engine.run_one_compaction_pass().unwrap(),
        crate::CompactionOutcome::Idle
    );
    engine
        .start_foreground(crate::engine::Work::CompactRange {
            lower: key_of(b"a"),
            upper: key_of(b"z"),
        })
        .wait()
        .unwrap();
    assert!(engine.current_version().levels[0].is_empty(), "compacted");
    assert_eq!(
        entries(&engine),
        1,
        "the pinned snapshot keeps the deletion"
    );
    match engine.check_early(&read_of_b(snapshot), &[]).unwrap() {
        EarlyVerdict::Conflict(conflict) => {
            assert_eq!(conflict.key(), key_of(b"b").as_slice());
            assert_eq!(conflict.latest_seq(), snapshot + 1);
        }
        EarlyVerdict::Marks(_) => panic!("the check missed the deletion of b"),
    }
    engine.release_snapshot(pin);
}

/// A commit the check at the horizon settles never enters the pipeline: it
/// aborts without taking a slot of the commit ring.
#[test]
fn a_conflict_found_at_the_horizon_does_not_enter_the_pipeline() {
    let (_dir, engine) = open();
    let snapshot = engine.snapshot_seq();
    apply(&engine, &Step::Put(0, 1));
    let checks = ValidationSet {
        reads: vec![ConflictKey {
            key: key_of(b"a"),
            observed_seq: snapshot,
            found: false,
            access: Access::ReadForUpdate,
            rule: ReadRule::Seq,
        }],
        writes_at: Some(snapshot),
        blind_merges_commute: false,
        exempt: Vec::new(),
        ranges: Vec::new(),
    };
    let taken = engine.pipeline_slots_taken();
    let mut points = std::collections::BTreeMap::new();
    points.insert(key_of(b"a"), Some(b"mine".to_vec()));
    let committed = engine.commit_optimistic(
        checks,
        points,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        DurabilityMode::Eventual,
    );
    let CommitOutcome::Conflict(conflict) = committed.unwrap() else {
        panic!("a stale read for update conflicts");
    };
    assert_eq!(conflict.key(), key_of(b"a").as_slice());
    assert_eq!(conflict.mine(), Access::ReadForUpdate);
    assert_eq!(
        engine.pipeline_slots_taken(),
        taken,
        "the commit took a slot of the ring before it aborted"
    );
}

/// With nothing committed since the snapshot the check at the horizon reads
/// nothing, and deciding the slot looks only at the memtables: a key whose versions
/// all sit in a table is never looked up there.
#[test]
fn a_commit_nothing_landed_under_never_reads_a_table() {
    let (_dir, engine) = open();
    apply(&engine, &Step::Put(0, 0));
    engine.flush_active_memtable().unwrap();
    let snapshot = engine.snapshot_seq();
    let (checks, ops) = commit_of(
        &Shape {
            reads: vec![(0, 0)],
            scan: Some((0, 3)),
            writes: vec![(0, Step::Put(0, 1)), (1, Step::Merge(0, 0))],
            blind: true,
            exempt: None,
        },
        snapshot,
    );
    crate::PerfContext::set_level(crate::PerfLevel::EnableCount);
    crate::PerfContext::reset();
    let (outcome, settled_early) = split_commit(&engine, checks, ops, || {});
    let lookups = crate::PerfContext::capture().block_cache_lookup_count;
    crate::PerfContext::set_level(crate::PerfLevel::Disable);
    assert!(!settled_early);
    assert!(matches!(outcome, Outcome::Landed { .. }), "{outcome:?}");
    assert_eq!(lookups, 0, "the commit read a table block");
}

/// Nothing was handed a sequence above the horizon of a commit's early
/// check, so deciding its slot reads nothing, not even the view
/// (`ceiling_skips_the_check_above` in `PipelineCheck.lean`). Once a write
/// lands between the early check and the slot, the slot is checked above
/// the horizon, and still commits when the write touched nothing it read.
#[test]
fn a_slot_nothing_was_decided_above_reads_nothing_when_it_is_decided() {
    use std::cell::Cell;
    use std::rc::Rc;

    let (_dir, engine) = open();
    apply(&engine, &Step::Put(0, 0));
    let checked = Rc::new(Cell::new(0));
    let counter = Rc::clone(&checked);
    super::hook::set(move |stage| {
        if stage == super::hook::Stage::Validate {
            counter.set(counter.get() + 1);
        }
    });
    let shape = Shape {
        reads: vec![(0, 0)],
        scan: Some((0, 1)),
        writes: vec![(0, Step::Put(0, 1))],
        blind: false,
        exempt: None,
    };
    let (checks, ops) = commit_of(&shape, engine.snapshot_seq());
    let (alone, _) = split_commit(&engine, checks, ops, || {});
    let checked_alone = checked.get();
    let (checks, ops) = commit_of(&shape, engine.snapshot_seq());
    let (behind, _) = split_commit(&engine, checks, ops, || apply(&engine, &Step::Put(2, 1)));
    super::hook::clear();
    assert!(matches!(alone, Outcome::Landed { .. }), "{alone:?}");
    assert_eq!(
        checked_alone, 0,
        "a slot nothing was decided above read the view"
    );
    assert!(matches!(behind, Outcome::Landed { .. }), "{behind:?}");
    assert_eq!(
        checked.get(),
        1,
        "a slot a write was decided above was not checked above its horizon"
    );
}
