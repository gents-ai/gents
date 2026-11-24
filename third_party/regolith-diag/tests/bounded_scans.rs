//! Range scans hand the iterator their end, so skipping deleted entries
//! stops there. These pin what that must not change: every scan still
//! returns exactly the live keys in `[start, end)`, merged with a
//! transaction's own writes, whatever is stored or buffered past `end`.

#![cfg(not(target_arch = "wasm32"))]

use regolith::{OptimisticTransactionDb, Options, TxnOptions};
use tempfile::TempDir;

/// Live `a/0..a/4`, then deleted `b/0000..b/0999`, then live `c/0`.
fn db_with_tombstones_after_a(dir: &TempDir) -> OptimisticTransactionDb {
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    for i in 0..5 {
        db.db().put(format!("a/{i}").as_bytes(), b"live").unwrap();
    }
    for i in 0..1_000 {
        let key = format!("b/{i:04}");
        db.db().put(key.as_bytes(), b"gone").unwrap();
        db.db().delete(key.as_bytes()).unwrap();
    }
    db.db().put(b"c/0", b"live").unwrap();
    db
}

fn keys<V>(entries: impl IntoIterator<Item = (Vec<u8>, V)>) -> Vec<String> {
    entries
        .into_iter()
        .map(|(key, _)| String::from_utf8(key).unwrap())
        .collect()
}

/// The keys of a transaction scan, whose items carry their errors.
fn txn_keys(
    stream: impl IntoIterator<Item = regolith::TxResult<(Vec<u8>, regolith::DbSlice)>>,
) -> Vec<String> {
    stream
        .into_iter()
        .map(|item| String::from_utf8(item.unwrap().0).unwrap())
        .collect()
}

fn a_keys() -> Vec<String> {
    (0..5).map(|i| format!("a/{i}")).collect()
}

#[test]
fn a_transaction_scan_returns_its_range_and_no_further() {
    let dir = TempDir::new().unwrap();
    let db = db_with_tombstones_after_a(&dir);
    let txn = db.begin(&TxnOptions::new());
    let mut stream = txn.scan_stream(Some(b"a/"), Some(b"b/"));
    assert_eq!(txn_keys(&mut stream), a_keys());
}

#[test]
fn a_transaction_scan_without_a_start_still_stops_at_its_end() {
    let dir = TempDir::new().unwrap();
    let db = db_with_tombstones_after_a(&dir);
    let txn = db.begin(&TxnOptions::new());
    let mut stream = txn.scan_stream(None, Some(b"b/"));
    assert_eq!(txn_keys(&mut stream), a_keys());
}

#[test]
fn a_transaction_scan_merges_its_writes_on_both_sides_of_the_bound() {
    let dir = TempDir::new().unwrap();
    let db = db_with_tombstones_after_a(&dir);
    let txn = db.begin(&TxnOptions::new());
    txn.put(b"a/9", b"mine").unwrap();
    txn.delete(b"a/0").unwrap();
    txn.put(b"b/0500", b"past the end").unwrap();
    let mut stream = txn.scan_stream(Some(b"a/"), Some(b"b/"));
    assert_eq!(txn_keys(&mut stream), ["a/1", "a/2", "a/3", "a/4", "a/9"]);

    let mut past = txn.scan_stream(Some(b"b/"), None);
    assert_eq!(txn_keys(&mut past), ["b/0500", "c/0"]);
}

#[test]
fn snapshot_and_db_scans_return_their_range_and_no_further() {
    let dir = TempDir::new().unwrap();
    let db = db_with_tombstones_after_a(&dir);
    let rows = db
        .db()
        .snapshot()
        .into_scan_stream(Some(b"a/"), Some(b"b/"))
        .collect::<regolith::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(keys(rows), a_keys());
    assert_eq!(
        keys(db.db().scan(Some(b"a/"), Some(b"b/")).unwrap()),
        a_keys()
    );
    assert_eq!(
        keys(db.db().scan(Some(b"b/"), Some(b"c/")).unwrap()),
        Vec::<String>::new()
    );
    assert_eq!(
        keys(db.db().scan(None, Some(b"a/2")).unwrap()),
        ["a/0", "a/1"]
    );
}

#[test]
fn a_bounded_cursor_ends_at_its_bound() {
    let dir = TempDir::new().unwrap();
    let db = db_with_tombstones_after_a(&dir);
    let mut cursor = db.db().snapshot().into_owned_iter();
    cursor.seek_bounded(b"a/3", b"b/");
    let mut seen = Vec::new();
    while cursor.valid() {
        seen.push(String::from_utf8(cursor.key().unwrap().to_vec()).unwrap());
        cursor.next();
    }
    assert_eq!(seen, ["a/3", "a/4"]);
    cursor.status().unwrap();
}
