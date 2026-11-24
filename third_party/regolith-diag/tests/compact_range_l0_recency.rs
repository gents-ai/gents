//! A bounded `compact_range` must not leave an older L0 table in front of a
//! newer one it moved down.
//!
//! Recency inside L0 is position: a lookup walks the level from the newest
//! table back and takes the first match without comparing sequence numbers.
//! A table that moves to L1 while an older table sharing a key with it stays
//! in L0 puts the older value in front of the newer one.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem, neither of which exists there.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{Db, OptimisticTransactionDb, Options, TransactionError, TxnOptions};
use tempfile::TempDir;

fn options() -> Options {
    Options::default().max_background_compactions(0)
}

#[test]
fn a_bounded_compact_range_does_not_put_an_old_version_above_a_new_one() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();

    db.put(b"k16", b"old").unwrap();
    db.flush().unwrap();
    db.put(b"k72", b"x").unwrap();
    db.put(b"k16", b"new").unwrap();
    db.flush().unwrap();

    // Only the newer table, k16..k72, intersects the range. The older one
    // holds k16 alone, outside it.
    db.compact_range(Some(b"k18"), Some(b"k35")).wait().unwrap();

    assert_eq!(db.get(b"k16").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(db.get(b"k72").unwrap().as_deref(), Some(&b"x"[..]));
}

#[test]
fn an_older_table_that_overlaps_only_through_another_older_one_moves_too() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();

    // Only the newest table intersects the range. The middle one overlaps it
    // at k55, which widens the picked range down to k40, where the oldest
    // table, k40 alone, joins.
    db.put(b"k40", b"oldest").unwrap();
    db.flush().unwrap();
    db.put(b"k40", b"middle").unwrap();
    db.put(b"k55", b"old").unwrap();
    db.flush().unwrap();
    db.put(b"k50", b"p").unwrap();
    db.put(b"k55", b"new").unwrap();
    db.put(b"k60", b"p").unwrap();
    db.flush().unwrap();

    db.compact_range(Some(b"k58"), Some(b"k70")).wait().unwrap();

    assert_eq!(db.get(b"k55").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(db.get(b"k40").unwrap().as_deref(), Some(&b"middle"[..]));
}

#[test]
fn a_table_that_starts_where_the_picked_range_ends_moves_too() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();

    // The older table's smallest key is the newer table's largest key, the
    // one key they share.
    db.put(b"k30", b"old").unwrap();
    db.put(b"k90", b"old").unwrap();
    db.flush().unwrap();
    db.put(b"k10", b"x").unwrap();
    db.put(b"k30", b"new").unwrap();
    db.flush().unwrap();

    db.compact_range(Some(b"k10"), Some(b"k20")).wait().unwrap();

    assert_eq!(db.get(b"k30").unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn a_transaction_that_read_a_key_still_conflicts_after_a_bounded_compact_range() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();

    db.db().put(b"k16", b"old").unwrap();
    db.db().flush().unwrap();

    let tx = db.begin(&TxnOptions::new());
    assert_eq!(tx.get(b"k16").unwrap().as_deref(), Some(&b"old"[..]));

    db.db().put(b"k72", b"x").unwrap();
    db.db().put(b"k16", b"new").unwrap();
    db.db().flush().unwrap();
    db.db()
        .compact_range(Some(b"k18"), Some(b"k35"))
        .wait()
        .unwrap();

    tx.put(b"k16", b"mine").unwrap();
    assert!(
        matches!(tx.commit(), Err(TransactionError::Conflict { .. })),
        "the concurrent write to k16 must abort the commit"
    );
    assert_eq!(db.db().get(b"k16").unwrap().as_deref(), Some(&b"new"[..]));
}
