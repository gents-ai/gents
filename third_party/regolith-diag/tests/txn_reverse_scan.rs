//! Reverse iteration over a transaction that has pending writes.
//!
//! The snapshot side has walked backwards for a long time; what had no reverse
//! form was the merge that overlays a transaction's own uncommitted writes on
//! top of it. These check that both sides reverse together: a buffered put
//! still replaces the snapshot entry it shadows, a buffered delete still hides
//! one, and the walk stops at the bound it is running towards.

use regolith::{IsolationLevel, OptimisticTransactionDb, Options, ScanDirection, TxnOptions};

fn db(dir: &std::path::Path) -> OptimisticTransactionDb {
    let db = OptimisticTransactionDb::open(dir, Options::default()).unwrap();
    for key in ["a", "b", "c", "d", "e"] {
        db.db().put(key.as_bytes(), b"committed").unwrap();
    }
    db
}

fn collect(stream: regolith::TxnScanStream<'_>) -> Vec<(String, String)> {
    stream
        .map(|item| {
            let (key, value) = item.unwrap();
            (
                String::from_utf8(key).unwrap(),
                String::from_utf8(value.to_vec()).unwrap(),
            )
        })
        .collect()
}

fn keys(stream: regolith::TxnScanStream<'_>) -> Vec<String> {
    collect(stream).into_iter().map(|(key, _)| key).collect()
}

#[test]
fn reverse_returns_the_range_in_descending_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    assert_eq!(
        keys(txn.scan_stream_in(None, None, ScanDirection::Reverse)),
        ["e", "d", "c", "b", "a"]
    );
}

/// Forward and reverse are the same set, in opposite orders.
#[test]
fn forward_and_reverse_agree_on_the_set() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"bb", b"pending").unwrap();
    txn.delete(b"d").unwrap();

    let forward = keys(txn.scan_stream_in(None, None, ScanDirection::Forward));
    let mut backward = keys(txn.scan_stream_in(None, None, ScanDirection::Reverse));
    backward.reverse();
    assert_eq!(forward, backward);
}

/// A pending put the snapshot has never seen appears in reverse order too.
#[test]
fn reverse_sees_a_pending_insert() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"bb", b"pending").unwrap();

    assert_eq!(
        keys(txn.scan_stream_in(None, None, ScanDirection::Reverse)),
        ["e", "d", "c", "bb", "b", "a"]
    );
}

/// A pending put over a committed key wins, in reverse as in forward.
#[test]
fn reverse_prefers_a_pending_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"c", b"pending").unwrap();

    let entries = collect(txn.scan_stream_in(None, None, ScanDirection::Reverse));
    assert_eq!(
        entries,
        [
            ("e".into(), "committed".into()),
            ("d".into(), "committed".into()),
            ("c".into(), "pending".into()),
            ("b".into(), "committed".into()),
            ("a".into(), "committed".into()),
        ]
    );
}

/// A pending delete hides the committed entry, in reverse as in forward.
#[test]
fn reverse_hides_a_pending_delete() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.delete(b"c").unwrap();

    assert_eq!(
        keys(txn.scan_stream_in(None, None, ScanDirection::Reverse)),
        ["e", "d", "b", "a"]
    );
}

/// The range is `[start, end)` in both directions: `start` is included and
/// `end` is not, whichever end the walk begins from.
#[test]
fn reverse_honours_the_same_half_open_range() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    assert_eq!(
        keys(txn.scan_stream_in(Some(b"b"), Some(b"d"), ScanDirection::Reverse)),
        ["c", "b"]
    );
    assert_eq!(
        keys(txn.scan_stream_in(Some(b"b"), Some(b"d"), ScanDirection::Forward)),
        ["b", "c"]
    );
}

/// The lone-entry-at-the-exclusive-end case, which is where an off-by-one in
/// the initial positioning shows up.
#[test]
fn reverse_excludes_a_single_entry_sitting_at_the_end_bound() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    db.db().put(b"c", b"committed").unwrap();
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    assert!(keys(txn.scan_stream_in(None, Some(b"c"), ScanDirection::Reverse)).is_empty());
}

/// Pending writes outside the range stay outside it.
#[test]
fn reverse_ignores_pending_writes_beyond_the_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"aa", b"pending").unwrap();
    txn.put(b"zz", b"pending").unwrap();

    assert_eq!(
        keys(txn.scan_stream_in(Some(b"b"), Some(b"d"), ScanDirection::Reverse)),
        ["c", "b"]
    );
}

/// A reverse walk that runs off the low end of the range terminates rather
/// than restarting at the top, which is what an invalid cursor handed to
/// `Entries` used to do.
#[test]
fn reverse_terminates_at_the_low_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    let walked = keys(txn.scan_stream_in(None, None, ScanDirection::Reverse));
    assert_eq!(walked.len(), 5, "stopped once, not restarted: {walked:?}");
}

/// A transaction with nothing but pending writes still scans backwards.
#[test]
fn reverse_over_pending_writes_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    for key in ["a", "b", "c"] {
        txn.put(key.as_bytes(), b"pending").unwrap();
    }

    assert_eq!(
        keys(txn.scan_stream_in(None, None, ScanDirection::Reverse)),
        ["c", "b", "a"]
    );
}

/// `scan_stream` keeps its forward meaning.
#[test]
fn the_undirected_scan_is_still_forward() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    assert_eq!(keys(txn.scan_stream(None, None)), ["a", "b", "c", "d", "e"]);
}
