//! Extra the mutating-surface sweep probes: mutating surfaces the shipped sweep does not name.
//!
//! The contract is that refusing a write must not depend on whether the
//! write carries work. These cover the TTL wrapper, the transaction
//! commit path, `write_with_durability`, and the column-family
//! lifecycle calls.

use regolith::{
    Db, DbWithTtl, DurabilityMode, Error, OptimisticTransactionDb, Options, TxnOptions, WriteBatch,
    WriteOptions,
};
use tempfile::TempDir;

fn opts() -> Options {
    Options::default()
}

fn seeded(dir: &TempDir) {
    let db = Db::open(dir.path(), opts()).expect("open");
    db.put(b"k", b"v").expect("put");
    db.close().expect("close");
    drop(db);
}

#[test]
fn a_read_only_handle_refuses_no_work_writes_on_every_remaining_surface() {
    let dir = TempDir::new().expect("tempdir");
    seeded(&dir);
    let ro = Db::open_read_only(dir.path(), opts()).expect("open_read_only");
    let cf = ro.default_cf();

    let mut bad = Vec::new();
    let mut check = |what: &str, got: regolith::Result<()>| {
        if !matches!(got, Err(Error::ReadOnly)) {
            bad.push(format!("{what}: expected Error::ReadOnly, got {got:?}"));
        }
    };

    check(
        "write_with_durability(empty)",
        ro.write_with_durability(WriteBatch::new(), DurabilityMode::Immediate),
    );
    check(
        "write_opt(empty, no_slowdown)",
        ro.write_opt(
            &{
                let mut w = WriteOptions::new();
                w.no_slowdown = true;
                w
            },
            WriteBatch::new(),
        ),
    );
    check("merge(empty operand)", ro.merge(b"k", b""));
    check("merge_cf(empty operand)", ro.merge_cf(&cf, b"k", b""));
    check("put(empty key, empty value)", ro.put(b"", b""));
    check("put_cf(empty key)", ro.put_cf(&cf, b"", b""));
    check("delete(empty key)", ro.delete(b""));
    check("delete_cf(empty key)", ro.delete_cf(&cf, b""));
    check(
        "delete_range_cf(start > end)",
        ro.delete_range_cf(&cf, b"z", b"a"),
    );
    check(
        "create_column_family",
        ro.create_column_family("x").map(|_| ()),
    );

    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

#[test]
fn a_closed_handle_refuses_no_work_writes_on_every_remaining_surface() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    db.put(b"k", b"v").expect("put");
    let cf = db.default_cf();
    db.close().expect("close");

    let mut bad = Vec::new();
    let mut check = |what: &str, got: regolith::Result<()>| {
        if !matches!(got, Err(Error::Closed)) {
            bad.push(format!("{what}: expected Error::Closed, got {got:?}"));
        }
    };

    check(
        "write_with_durability(empty)",
        db.write_with_durability(WriteBatch::new(), DurabilityMode::Immediate),
    );
    check("merge(empty operand)", db.merge(b"k", b""));
    check("put(empty key/value)", db.put(b"", b""));
    check("delete(empty key)", db.delete(b""));
    check("delete_cf(empty key)", db.delete_cf(&cf, b""));
    check(
        "create_column_family",
        db.create_column_family("x").map(|_| ()),
    );

    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// The TTL wrapper delegates its writes; an empty batch through it must
/// refuse for the same reason a loaded one does.
#[test]
fn a_closed_ttl_handle_refuses_an_empty_batch() {
    let dir = TempDir::new().expect("tempdir");
    let ttl = DbWithTtl::open(dir.path(), opts(), 3600).expect("open ttl");
    ttl.put(b"k", b"v").expect("put");
    ttl.close().expect("close");

    let empty = ttl.write(WriteBatch::new());
    let mut loaded_batch = WriteBatch::new();
    loaded_batch.put(b"a", b"b");
    let loaded = ttl.write(loaded_batch);
    assert!(
        matches!(loaded, Err(Error::Closed)),
        "a loaded batch on a closed TTL handle must be Closed, got {loaded:?}",
    );
    assert!(
        matches!(empty, Err(Error::Closed)),
        "an empty batch on a closed TTL handle must be Closed too, got {empty:?}",
    );
}

/// A transaction that buffered nothing must not commit successfully
/// against a closed database.
#[test]
fn a_transaction_that_carries_no_work_still_fails_to_commit_when_closed() {
    let dir = TempDir::new().expect("tempdir");
    let tdb = OptimisticTransactionDb::open(dir.path(), opts()).expect("open");
    tdb.db().put(b"k", b"v").expect("put");
    let empty = tdb.begin(&TxnOptions::new());
    let loaded = tdb.begin(&TxnOptions::new());
    loaded.put(b"a", b"b").expect("buffer");
    tdb.db().close().expect("close");

    let loaded_res = loaded.commit();
    let empty_res = empty.commit();
    assert!(
        loaded_res.is_err(),
        "a loaded transaction must not commit into a closed database"
    );
    assert!(
        empty_res.is_err(),
        "an empty transaction must fail for the same reason a loaded one does, got \
         {empty_res:?} while the loaded one gave {loaded_res:?}",
    );
}
