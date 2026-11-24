//! Adversarial probes for the "a write that carries no work is still a
//! write" contract (the mutating-surface sweep).
//!
//! The contract has to hold for **every** mutating entry point and for
//! both refusal reasons, otherwise a caller can probe a handle with an
//! empty payload, get `Ok(())`, and conclude the handle is writable.
//! These tests sweep the entry points rather than the three the original
//! gate names, and pair each no-work call with a loaded control so a
//! rejection that happens for the wrong reason is visible.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{Db, Error, Options, WriteBatch, WriteOptions};
use tempfile::TempDir;

fn opts() -> Options {
    Options::default().write_buffer_size(64 * 1024)
}

fn assert_read_only(what: &str, got: regolith::Result<()>) {
    match got {
        Err(Error::ReadOnly) => {}
        other => panic!("{what}: expected Error::ReadOnly, got {other:?}"),
    }
}

fn assert_closed(what: &str, got: regolith::Result<()>) {
    match got {
        Err(Error::Closed) => {}
        other => panic!("{what}: expected Error::Closed, got {other:?}"),
    }
}

/// Every mutating entry point, called with a payload that carries no
/// work, against a read-only handle. Each is paired with the same call
/// carrying real work, so an entry point that refuses for an unrelated
/// reason cannot pass by accident.
#[test]
fn every_no_work_write_is_refused_by_a_read_only_handle() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    for i in 0..20u32 {
        regolith::through_stalls(|| db.put(&i.to_be_bytes(), b"v")).expect("put");
    }
    db.close().expect("close");
    drop(db);

    let ro = Db::open_read_only(dir.path(), opts()).expect("open_read_only");
    let cf = ro.default_cf();

    assert_read_only("write(empty)", ro.write(WriteBatch::new()));
    assert_read_only(
        "write_opt(empty)",
        ro.write_opt(&WriteOptions::new(), WriteBatch::new()),
    );
    assert_read_only("delete_range(start == end)", ro.delete_range(b"k", b"k"));
    assert_read_only("delete_range(start > end)", ro.delete_range(b"z", b"a"));
    assert_read_only(
        "delete_range_opt(start == end)",
        ro.delete_range_opt(&WriteOptions::new(), b"k", b"k"),
    );
    assert_read_only(
        "delete_range_cf(start == end)",
        ro.delete_range_cf(&cf, b"k", b"k"),
    );
    assert_read_only(
        "ingest_external_files(empty list)",
        ro.ingest_external_files(&[], Default::default()).wait(),
    );

    // Loaded controls: the same entry points with real work must refuse
    // for the same reason, not a different one.
    let mut loaded = WriteBatch::new();
    loaded.put(b"a", b"b");
    assert_read_only("write(loaded)", ro.write(loaded));
    assert_read_only("delete_range(loaded)", ro.delete_range(b"a", b"z"));
    assert_read_only("put", ro.put(b"a", b"b"));
    assert_read_only("delete", ro.delete(b"a"));
    assert_read_only("drop_all", ro.drop_all());
    assert_read_only("compact_range", ro.compact_range(None, None).wait());

    // The handle still reads.
    assert_eq!(
        ro.get(&5u32.to_be_bytes()).expect("get"),
        Some(b"v".to_vec())
    );
}

/// The same sweep against a *closed* handle: `Error::Closed` must win
/// over the no-work short-circuit too, and must win over
/// `Error::ReadOnly` when both apply.
#[test]
fn every_no_work_write_is_refused_by_a_closed_handle() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    regolith::through_stalls(|| db.put(b"k", b"v")).expect("put");
    let cf = db.default_cf();
    db.close().expect("close");

    assert_closed("write(empty)", db.write(WriteBatch::new()));
    assert_closed(
        "write_opt(empty)",
        db.write_opt(&WriteOptions::new(), WriteBatch::new()),
    );
    assert_closed("delete_range(start == end)", db.delete_range(b"k", b"k"));
    assert_closed(
        "delete_range_opt(start == end)",
        db.delete_range_opt(&WriteOptions::new(), b"k", b"k"),
    );
    assert_closed(
        "delete_range_cf(start == end)",
        db.delete_range_cf(&cf, b"k", b"k"),
    );
    assert_closed(
        "ingest_external_files(empty list)",
        db.ingest_external_files(&[], Default::default()).wait(),
    );

    // A closed read-only handle must report Closed, not ReadOnly.
    let dir2 = TempDir::new().expect("tempdir");
    let seed = Db::open(dir2.path(), opts()).expect("open");
    regolith::through_stalls(|| seed.put(b"k", b"v")).expect("put");
    seed.close().expect("close");
    drop(seed);
    let ro = Db::open_read_only(dir2.path(), opts()).expect("open_read_only");
    ro.close().expect("close");
    assert_closed("closed read-only write(empty)", ro.write(WriteBatch::new()));
    assert_closed(
        "closed read-only delete_range(start == end)",
        ro.delete_range(b"k", b"k"),
    );
}
