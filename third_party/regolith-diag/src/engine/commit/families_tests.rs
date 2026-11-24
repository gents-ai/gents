//! The column-family fence on every path a write takes into the pipeline.
//!
//! Each test makes a write that passed every check made before the pipeline
//! (the database's own handle check included) meet the drop of the family it
//! names only in ring order. The writer is stopped at a stage of the
//! pipeline with a test hook while the drop runs on another thread. Stopped
//! before it installed its slot, its reservation is voided by the drop and
//! it takes its turn behind the drop: it must be refused with
//! `InvalidColumnFamily`, take no sequence and leave nothing in the family's
//! key range. Stopped after, the drop decides the write first, at a lower
//! sequence, and its tombstone deletes it. Either way nothing is left in the
//! family, which is the contract (`proofs/tla/CfRegistry.tla`).

use std::collections::BTreeMap;
use std::io;
use std::sync::mpsc;

use tempfile::TempDir;

use super::super::super::{DurabilityMode, RegolithEngine, ValidationSet};
use super::super::hook::{self, Stage};
use super::super::{NowaitCommit, Settled, TxnCommit};
use crate::column_family::{ColumnFamilyHandle, DEFAULT_CF_ID, cf_lower_bound, cf_upper_bound};
use crate::{Db, Error, IngestOptions, Options, SstFileWriter, WriteBatchOp, prefix_key};

/// A database with no worker, so no background thread takes a turn in the
/// pipeline or the compaction gate, and one live family to drop.
fn open() -> (TempDir, Db, ColumnFamilyHandle) {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default().max_background_compactions(0)).unwrap();
    let cf = db.create_column_family("doomed").unwrap();
    (dir, db, cf)
}

fn put(id: u32, key: &[u8]) -> Vec<WriteBatchOp> {
    vec![WriteBatchOp::Put {
        key: prefix_key(id, key),
        value: b"v".to_vec(),
    }]
}

/// Entries visible at the newest sequence in family `id`'s key range, read
/// below the column-family layer: the only place a write that slipped past
/// a drop could still be seen.
fn in_family(engine: &RegolithEngine, id: u32) -> usize {
    crate::collect_range(
        engine.new_iter_latest(),
        Some(&cf_lower_bound(id)),
        Some(&cf_upper_bound(id)),
    )
    .unwrap()
    .len()
}

fn assert_refused(outcome: io::Result<impl std::fmt::Debug>, path: &str) {
    match outcome.map_err(Error::from) {
        Err(Error::InvalidColumnFamily(_)) => {}
        other => panic!("{path}: a write to a dropped family was not refused: {other:?}"),
    }
}

/// Run `call` on a scoped thread, stop it the first time it reaches `stage`
/// of the pipeline, run `meanwhile` on this thread, then let it go on and
/// return what it returned.
fn stopped_at<T: Send>(
    stage: Stage,
    call: impl FnOnce() -> T + Send,
    meanwhile: impl FnOnce(),
) -> T {
    let (reached, at_stage) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let stopped = scope.spawn(move || {
            let mut fired = false;
            hook::set(move |now| {
                if now == stage && !fired {
                    fired = true;
                    reached.send(()).unwrap();
                    resumed.recv().unwrap();
                }
            });
            let out = call();
            hook::clear();
            out
        });
        at_stage
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the call never reached the stage");
        meanwhile();
        resume.send(()).unwrap();
        stopped.join().unwrap()
    })
}

/// A write that passed the handle check before the drop takes its turn
/// after it.
#[test]
fn a_write_to_a_dropped_family_is_refused() {
    let (_dir, db, cf) = open();
    let engine = &db.engine;
    let id = cf.id();
    db.drop_column_family(cf).unwrap();
    let before = engine.last_decided_seq();
    assert_refused(
        engine.apply_batch(put(id, b"late"), DurabilityMode::Eventual, false),
        "a plain write",
    );
    assert_eq!(
        engine.last_decided_seq(),
        before,
        "a refused write took a sequence"
    );
    assert_eq!(in_family(engine, id), 0);
}

/// A writer stopped between its reservation and its install: the drop voids
/// the reservation, the writer reserves again behind the drop, and is
/// fenced there. A write to a live family goes on as usual.
#[test]
fn a_write_whose_reservation_the_drop_voided_is_refused_behind_it() {
    let (_dir, db, cf) = open();
    let engine = &db.engine;
    let id = cf.id();
    let late = stopped_at(
        Stage::Reserve,
        || engine.apply_batch(put(id, b"late"), DurabilityMode::Eventual, false),
        || db.drop_column_family(cf.clone()).unwrap(),
    );
    assert_refused(late, "a voided reservation");
    assert_eq!(in_family(engine, id), 0);
    engine
        .apply_batch(put(DEFAULT_CF_ID, b"live"), DurabilityMode::Eventual, false)
        .unwrap();
    assert_eq!(
        engine
            .get(&prefix_key(DEFAULT_CF_ID, b"live"), u64::MAX)
            .unwrap(),
        Some(b"v".to_vec())
    );
}

/// A writer stopped after it installed its slot and before it decided it:
/// the drop, behind it in the ring, decides the write first, while the
/// family is live. The write lands at a lower sequence than the tombstone,
/// which deletes it.
#[test]
fn a_write_decided_before_the_drop_is_deleted_by_it() {
    let (_dir, db, cf) = open();
    let engine = &db.engine;
    let id = cf.id();
    let before = engine.last_decided_seq();
    let early = stopped_at(
        Stage::Decide,
        || engine.apply_batch(put(id, b"early"), DurabilityMode::Eventual, false),
        || db.drop_column_family(cf.clone()).unwrap(),
    );
    let seq = early.expect("a write ordered before the drop lands");
    assert_eq!(seq, before + 1, "it took the sequence before the drop's");
    assert_eq!(
        engine.get(&prefix_key(id, b"early"), seq).unwrap(),
        Some(b"v".to_vec()),
        "visible at its own sequence, below the tombstone"
    );
    assert_eq!(in_family(engine, id), 0, "and deleted above it");
}

/// An optimistic transaction's commit whose reservation the drop voided:
/// fenced when its slot is decided, before it is validated.
#[test]
fn a_transaction_ordered_behind_the_drop_is_refused() {
    let (_dir, db, cf) = open();
    let engine = &db.engine;
    let id = cf.id();
    let snapshot = engine.visible_seq.visible();
    let late = stopped_at(
        Stage::Reserve,
        || {
            let mut points = BTreeMap::new();
            points.insert(prefix_key(id, b"txn"), Some(b"v".to_vec()));
            engine.commit_optimistic(
                ValidationSet {
                    reads: Vec::new(),
                    writes_at: Some(snapshot),
                    blind_merges_commute: false,
                    exempt: Vec::new(),
                    ranges: Vec::new(),
                },
                points,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                DurabilityMode::Eventual,
            )
        },
        || db.drop_column_family(cf.clone()).unwrap(),
    );
    assert_refused(late, "a transaction");
    assert_eq!(in_family(engine, id), 0);
}

/// The counter `allocate` reserves, a slot of its own kind.
#[test]
fn an_allocation_in_a_dropped_family_is_refused() {
    let (_dir, db, cf) = open();
    let engine = &db.engine;
    let id = cf.id();
    db.drop_column_family(cf).unwrap();
    assert_refused(engine.allocate(prefix_key(id, b"counter"), 3), "allocate");
    assert_eq!(in_family(engine, id), 0);
}

/// An ingest whose table was validated while the family was live, and whose
/// install takes its turn after the drop.
#[test]
fn an_ingest_installed_after_the_drop_is_refused() {
    let (dir, db, cf) = open();
    let engine = &db.engine;
    let id = cf.id();
    let source = dir.path().join("external.sst");
    let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
    writer.put_cf(&cf, b"ingested", b"v").unwrap();
    writer.finish().unwrap();
    let tables = |engine: &RegolithEngine| {
        engine
            .published_version()
            .levels
            .iter()
            .map(Vec::len)
            .sum::<usize>()
    };
    let tables_before = tables(engine);

    // The ingest is stopped once its table is staged and validated, when it
    // reserves the slot of its install; the drop voids that reservation.
    let ingested = stopped_at(
        Stage::Reserve,
        || {
            db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default())
                .wait()
        },
        || engine.drop_family(&cf, DurabilityMode::Eventual).unwrap(),
    );
    match ingested {
        Err(Error::InvalidColumnFamily(_)) => {}
        other => panic!("ingest: a table for a dropped family was not refused: {other:?}"),
    }
    assert_eq!(in_family(engine, id), 0);
    assert_eq!(
        tables(engine),
        tables_before,
        "the refused table was installed"
    );
}

/// A transaction committed with `commit_nowait` that writes `key` in family
/// `id` at `Immediate` durability: it is decided and written, and left
/// waiting for its sync.
fn nowait_txn(engine: &RegolithEngine, id: u32, key: &[u8]) -> io::Result<NowaitCommit> {
    let mut points = BTreeMap::new();
    points.insert(prefix_key(id, key), Some(b"v".to_vec()));
    engine.commit_optimistic_nowait(TxnCommit {
        checks: ValidationSet {
            reads: Vec::new(),
            writes_at: Some(engine.visible_seq.visible()),
            blind_merges_commute: false,
            exempt: Vec::new(),
            ranges: Vec::new(),
        },
        point_ops: points,
        range_deletes: Vec::new(),
        merges: Vec::new(),
        appends: Vec::new(),
        durability: DurabilityMode::Immediate,
        read_queue: None,
    })
}

/// A commit left waiting for its sync that wrote to a family is published
/// before the family's drop runs: the drop is an ordered unit, and a unit
/// brings every commit before it through first. So the pending write is
/// published below the tombstone, and a write ordered behind the drop is
/// refused.
#[test]
fn a_drop_publishes_the_commit_left_pending_before_it_runs() {
    let (_dir, db, cf) = open();
    let engine = &db.engine;
    let id = cf.id();
    let pending = match nowait_txn(engine, id, b"owed").unwrap() {
        NowaitCommit::Pending(pending) => pending,
        NowaitCommit::Done(outcome) => {
            panic!("setup: the nowait commit did not wait for its sync: {outcome:?}")
        }
        NowaitCommit::Owed { .. } => panic!("setup: the nowait commit missed a block"),
    };
    let owed_seq = engine.last_decided_seq();
    assert!(
        engine.visible_seq.visible() < owed_seq,
        "setup: nothing published the pending commit yet"
    );
    db.drop_column_family(cf).unwrap();
    assert!(
        engine.visible_seq.visible() > owed_seq,
        "the drop ran with a commit left pending ahead of it"
    );
    let settled = pending.settled(None);
    assert!(
        matches!(settled, Ok(Settled::Committed { seq: Some(seq), .. }) if seq == owed_seq),
        "the pending commit did not land at its own sequence: {settled:?}"
    );
    // Visible at its own sequence, below the tombstone, and deleted above it.
    assert_eq!(
        engine.get(&prefix_key(id, b"owed"), owed_seq).unwrap(),
        Some(b"v".to_vec())
    );
    assert_eq!(in_family(engine, id), 0);
    assert_refused(
        engine.apply_batch(put(id, b"late"), DurabilityMode::Eventual, false),
        "a write behind the drop",
    );
}
