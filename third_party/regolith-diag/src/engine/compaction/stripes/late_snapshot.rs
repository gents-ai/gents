//! A snapshot that registers after a compaction pass has read the live
//! list must still read the version it began at.
//!
//! The pass reads the list once its inputs are fixed. Read before, a
//! snapshot registering after the read could sit beside a table flushed
//! after the snapshot, and the pass would fold the two into one stripe and
//! drop the version the snapshot reads. Each test puts that snapshot in
//! place by hand: the hook runs right after the list is taken, registers
//! the snapshot, overwrites the key and flushes the overwrite to a table of
//! its own.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tempfile::TempDir;

use crate::engine::snapshot_registry::after_next_live_read;
use crate::{CompactionOutcome, CompactionStyle, Db, Options, Snapshot};

/// A database that compacts only when a test asks, holding `k = v` in one
/// level-0 table and another key in a second.
fn two_tables(compaction_style: CompactionStyle) -> (TempDir, Arc<Db>) {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .compaction_style(compaction_style)
            .l0_compaction_trigger(2)
            .max_background_compactions(0),
    )
    .unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    db.put(b"other", b"x").unwrap();
    db.flush().unwrap();
    (dir, Arc::new(db))
}

/// Arm the hook, and return where the snapshot it registers will land.
fn register_then_overwrite(db: &Arc<Db>) -> Rc<RefCell<Option<Snapshot>>> {
    let landed = Rc::new(RefCell::new(None));
    let (held, slot) = (Arc::clone(db), Rc::clone(&landed));
    after_next_live_read(move || {
        *slot.borrow_mut() = Some(held.snapshot());
        held.put(b"k", b"w").unwrap();
        held.flush().unwrap();
    });
    landed
}

/// The snapshot the hook registered still reads `v`, and the head reads
/// the overwrite.
fn assert_snapshot_kept_its_version(db: &Db, landed: &RefCell<Option<Snapshot>>) {
    let snapshot = landed
        .borrow_mut()
        .take()
        .expect("the pass read the live list, which fired the hook");
    assert_eq!(snapshot.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"w"[..]));
}

#[test]
fn compact_step_keeps_a_snapshot_registered_after_its_live_read() {
    let (_dir, db) = two_tables(CompactionStyle::Level);
    let landed = register_then_overwrite(&db);

    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);

    assert_snapshot_kept_its_version(&db, &landed);
}

#[test]
fn compact_range_keeps_a_snapshot_registered_after_its_live_read() {
    let (_dir, db) = two_tables(CompactionStyle::Level);
    let landed = register_then_overwrite(&db);

    db.compact_range(None, None).wait().unwrap();

    assert_snapshot_kept_its_version(&db, &landed);
}

#[test]
fn a_universal_compact_range_keeps_a_snapshot_registered_after_its_live_read() {
    let (_dir, db) = two_tables(CompactionStyle::Universal);
    let landed = register_then_overwrite(&db);

    db.compact_range(None, None).wait().unwrap();

    assert_snapshot_kept_its_version(&db, &landed);
}
